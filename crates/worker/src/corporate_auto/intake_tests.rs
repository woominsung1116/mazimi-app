use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";

// ── request-free gates ────────────────────────────────────────────────────

#[test]
fn the_kill_switch_is_strict_and_defaults_to_shadow() {
    assert_eq!(parse_mode("").unwrap(), Mode::Shadow);
    assert_eq!(parse_mode(" Shadow ").unwrap(), Mode::Shadow);
    assert_eq!(parse_mode("off").unwrap(), Mode::Off);
    assert_eq!(parse_mode("live").unwrap(), Mode::Live);
    assert!(parse_mode("on").is_err());
    assert!(parse_mode("true").is_err());
    // Live intake needs the evidence job in live: only it shows an offer.
    assert_eq!(effective_mode(Mode::Live, VerifyMode::Shadow), Mode::Shadow);
    assert_eq!(
        effective_mode(Mode::Live, VerifyMode::HideOnly),
        Mode::Shadow
    );
    assert_eq!(effective_mode(Mode::Live, VerifyMode::Live), Mode::Live);
    assert_eq!(effective_mode(Mode::Off, VerifyMode::Live), Mode::Off);
}

/// `seeded`: live for reviewed seed URLs only, and only with the evidence
/// job in live; anything else reads it as shadow.
#[test]
fn seeded_is_one_mode_that_needs_the_evidence_job_in_live() {
    assert_eq!(parse_mode("seeded").unwrap(), Mode::Seeded);
    assert_eq!(parse_mode(" SEEDED ").unwrap(), Mode::Seeded);
    assert_eq!(effective_mode(Mode::Seeded, VerifyMode::Live), Mode::Seeded);
    assert_eq!(
        effective_mode(Mode::Seeded, VerifyMode::Shadow),
        Mode::Shadow
    );
    assert_eq!(
        effective_mode(Mode::Seeded, VerifyMode::HideOnly),
        Mode::Shadow
    );
    assert!(parse_mode("seed").is_err());
}

fn offer_page(body: &str) -> String {
    format!(
        "<html><head><title>Education</title></head><body><header><nav>Home Products</nav></header><main>{body}\
         <section><h2>About Live</h2><p>{}</p></section></main><footer><a href=\"/legal\">Legal</a></footer></body></html>",
        "Live is software for creating musical ideas, arranging songs and playing them on stage. "
            .repeat(6)
    )
}

const ABLETON: &str = "<section><h2>Students and teachers</h2>\
    <p>Students and teachers get a 50% discount on a new Live license.</p></section>";

/// The pure chain a page goes through before stability and dedup: reader A,
/// the policy gate's quote checks and reader B(a), then [`check_offer`].
fn decide(html: &str, url: &str, brand: &str) -> Result<Checked, String> {
    let today = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
    let offer = bootstrap::extract_offer(html, today).map_err(|r| format!("reader_a:{r}"))?;
    let text = evidence::visible_text(html);
    let decision = Decision {
        candidate_ids: vec!["c".into()],
        url: url.into(),
        final_url: None,
        domain: "example".into(),
        brand: Some(brand.into()),
        brand_id: None,
        domain_id: None,
        outcome: "proposal".into(),
        http_status: Some(200),
        page_title: None,
        offer: Some(offer),
        visible_chars: Some(text.chars().count()),
        page_hash: Some(PageEvidence::from_visible_text(&text).hash()),
        page_text: Some(text),
        page_html: Some(html.to_string()),
        scope: Default::default(),
    };
    let attempt = reextract::gate_offer(decision, json!({}), publish::PAGE_ENDING_SCAN);
    let Some(decision) = attempt.decision.filter(|_| attempt.outcome == "proposal") else {
        return Err(format!("{}:{}", gate_of(&attempt.outcome), attempt.outcome));
    };
    check_offer(&decision, brand, brand).map_err(|(gate, detail)| format!("{gate}:{detail}"))
}

#[test]
fn a_real_offer_passes_with_a_verbatim_display() {
    let checked = decide(
        &offer_page(ABLETON),
        "https://www.ableton.com/en/shop/education/",
        "Ableton",
    )
    .unwrap();
    assert_eq!(checked.title, "Ableton 학생 50% 할인");
    assert!(
        checked.summary.contains("50% discount"),
        "{}",
        checked.summary
    );
    assert!(checked.offer.audience.contains(&"student"));
    assert!(checked
        .quotes
        .iter()
        .all(|q| q.chars().count() <= bootstrap::MAX_QUOTE_CHARS));
}

/// Junk the bootstrap queue is full of: none of it may publish.
#[test]
fn junk_candidates_are_refused() {
    let cases: &[(&str, String)] = &[
        // A general membership: no student or youth audience.
        (
            "membership",
            offer_page("<section><h2>멤버십 등급 혜택</h2><p>골드 등급 회원은 음료 10% 할인 쿠폰을 받습니다.</p></section>"),
        ),
        // A book product page with a student topic in its title.
        (
            "product",
            offer_page("<section><h2>도서</h2><p>AI를 활용한 대학생 기초 글쓰기 정가 20,000원 판매가 18,000원 (10% 할인)</p><p>ISBN 9791100000000</p></section>"),
        ),
        // An educator-only offer: out of the student/youth scope.
        (
            "educators",
            offer_page("<section><h2>Educator pricing</h2><p>Teachers get a 40% discount on the annual plan.</p></section>"),
        ),
        // A condition that cannot be shown word for word (a collapsed panel).
        (
            "condition_hidden",
            offer_page(&format!(
                "{ABLETON}<div style=\"display:none\"><p>Excluding upgrades.</p></div>"
            )),
        ),
        // An ended offer.
        (
            "ended",
            offer_page("<section><h2>Students</h2><p>Students get a 50% discount. 이벤트가 종료되었습니다.</p></section>"),
        ),
        // Several offers on one page.
        (
            "multi_offer",
            offer_page("<section><h2>Students</h2><p>Students get a 10% discount.</p><p>Students get a 20% discount on bundles.</p><p>Students get a 30% discount on Suite.</p></section>"),
        ),
    ];
    for (label, html) in cases {
        let result = decide(html, "https://shop.example/student", "Example");
        assert!(
            result.is_err(),
            "{label} must be refused, got {:?}",
            result.map(|c| c.title)
        );
    }
}

fn fixture(file: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/real_pages")
            .join(file),
    )
    .unwrap()
}

/// Stored production pages (2026-10-18): every general membership or points
/// programme is refused; the student offers that pass carry their own facts.
#[test]
fn stored_real_pages_refuse_memberships_and_pass_only_student_offers() {
    let memberships = [
        (
            "0d2d2070.html",
            "https://www.ikea.com/kr/ko/ikea-family/benefits/",
            "IKEA",
        ),
        (
            "435bcc1e.html",
            "https://www.daiso.co.kr/cs/faq?cid=21",
            "다이소",
        ),
        (
            "f5a9c323.html",
            "https://www.starbucks.co.kr/msr/msreward/level_benefit.do",
            "스타벅스",
        ),
        (
            "cb304aab.html",
            "https://www.yes24.com/notice/FirstClass/infoYESPoint.aspx",
            "예스24",
        ),
        (
            "247e7e73.html",
            "https://www.coffeebeankorea.com/card/about.asp",
            "커피빈",
        ),
        (
            "40693033.html",
            "https://www.baristapaulbassett.co.kr/society/Introduction.pb",
            "폴 바셋",
        ),
        (
            "2ce025e1.html",
            "https://m.hollys.co.kr/customer/faq.do",
            "할리스",
        ),
    ];
    for (file, url, brand) in memberships {
        let result = decide(&fixture(file), url, brand);
        assert!(
            result.is_err(),
            "{brand} membership must be refused: {:?}",
            result.map(|c| c.summary)
        );
    }
    let offers = [
        ("18987f71.html", "https://1password.com/developers/students", "1Password"),
        ("f944afc6.html", "https://www.axure.com/edu", "Axure"),
        ("76add9b9.html", "https://bootstrapstudio.io/contact-us", "Bootstrap Studio"),
        ("d77ca111.html", "https://education.github.com/pack", "GitHub"),
        ("a35a0e9b.html", "https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers", "JetBrains"),
        ("4067fb7a.html", "https://www.microsoft.com/ko-kr/education/products/office", "Microsoft"),
        ("f02a666a.html", "https://azure.microsoft.com/en-us/free/students/", "Microsoft Azure"),
        ("56d6729d.html", "https://obsidian.md/help/discounts", "Obsidian"),
        ("901772a5.html", "https://polypane.app/special-offers/", "Polypane"),
        ("1249aeda.html", "https://www.sketch.com/education/", "Sketch"),
        ("c79c74a5.html", "https://termius.com/education", "Termius"),
        ("785fd0b2.html", "https://www.git-tower.com/students", "Tower"),
        ("5fa3af29.html", "https://www.nike.com/kr/membership/student-discount-program", "나이키"),
        ("ae07f4c8.html", "https://www.adobe.com/kr/education/students/creativecloud.html", "Adobe"),
        ("7d66fe55.html", "https://www.apple.com/kr-edu/store", "Apple"),
        ("0daf688b.html", "https://www.spotify.com/kr-ko/student/", "Spotify"),
    ];
    let mut passed = Vec::new();
    for (file, url, brand) in offers {
        match decide(&fixture(file), url, brand) {
            Ok(checked) => {
                assert!(checked
                    .offer
                    .audience
                    .iter()
                    .any(|a| STUDENT_YOUTH.contains(a)));
                assert!(checked.title.starts_with(brand), "{}", checked.title);
                eprintln!(
                    "PASS {brand}: {} | {}",
                    checked.title,
                    checked.summary.chars().take(160).collect::<String>()
                );
                passed.push(brand);
            }
            Err(reason) => eprintln!("REFUSED {brand}: {reason}"),
        }
    }
    // Measured, not tuned (§28): the pages whose shown text and conditions
    // the report lists word for word. A change here changes what users read.
    eprintln!("passed {}/{}", passed.len(), offers.len());
    assert_eq!(
        passed,
        vec![
            "Axure",
            "Bootstrap Studio",
            "Microsoft Azure",
            "Polypane",
            // Review L2: Sketch states one year and "for as long as the use
            // is part of your studies"; two terms, so it is refused.
            "Termius",
            "Tower"
        ]
    );
}

#[test]
fn the_offer_key_ignores_page_churn_and_follows_the_values() {
    let base = decide(&offer_page(ABLETON), "https://www.ableton.com/a", "Ableton").unwrap();
    let churned = offer_page(&format!(
        "{ABLETON}<section><h2>Live 12</h2><p>New devices and sounds.</p></section>"
    ));
    let again = decide(&churned, "https://www.ableton.com/a", "Ableton").unwrap();
    assert_eq!(base.offer_key, again.offer_key);
    let changed = decide(
        &offer_page(&ABLETON.replace("50%", "40%")),
        "https://www.ableton.com/a",
        "Ableton",
    )
    .unwrap();
    assert_ne!(base.offer_key, changed.offer_key);
    // Another brand with the same words is another offer.
    assert_ne!(offer_key("other", &base.offer), base.offer_key);
    assert_eq!(
        url_key("https://www.Ableton.com/en/shop/education/#x"),
        "ableton.com/en/shop/education"
    );
    assert_eq!(
        url_key("https://ableton.com/en/shop/education"),
        "ableton.com/en/shop/education"
    );
    assert_ne!(
        url_key("https://ableton.com/a?x=1"),
        url_key("https://ableton.com/a?x=2")
    );
}

// ── the job on a database ─────────────────────────────────────────────────

type Route = (u16, String, Option<String>);

#[derive(Clone, Default)]
struct Site {
    routes: Arc<Mutex<HashMap<String, Route>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Site {
    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), (status, body.into(), None));
    }
    fn hits(&self, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == path)
            .count()
    }
    fn total(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

async fn serve(site: Site) -> String {
    use axum::{extract::Request, response::IntoResponse, Router};
    let app = Router::new().fallback(move |request: Request| {
        let site = site.clone();
        async move {
            let path = request.uri().path().to_string();
            site.hits.lock().unwrap().push(path.clone());
            let route = site.routes.lock().unwrap().get(&path).cloned();
            match route {
                Some((status, body, location)) => {
                    let content_type = if path == "/robots.txt" {
                        "text/plain"
                    } else {
                        "text/html; charset=utf-8"
                    };
                    let mut response = (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", content_type)],
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

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn db() -> Option<PgPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    Some(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap(),
    )
}

struct Fixture {
    domain_id: Uuid,
    brand_id: Uuid,
}

async fn domain(pool: &PgPool) -> Fixture {
    let domain_id = Uuid::new_v4();
    let brand_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
         VALUES ('Ableton', $1, 'intake_test', $1, 50) RETURNING id",
    )
    .bind(format!("ableton{}", domain_id.simple()))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO benefit_source_domains (id, brand_id, organization_name, domain, source_role, verification_status,
                                             verification_reference, verified_at, compliance_status, collection_mode)
         VALUES ($1, $2, 'Ableton', $3, 'brand_official', 'verified', 'fixture', now(), 'pending', 'manual_review')",
    )
    .bind(domain_id)
    .bind(brand_id)
    .bind(format!("{}.intake.example", domain_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    Fixture {
        domain_id,
        brand_id,
    }
}

async fn candidate(pool: &PgPool, f: &Fixture, url: &str, branded: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
         VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'review', 'detail')",
    )
    .bind(id)
    .bind(f.domain_id)
    .bind(url)
    .execute(pool)
    .await
    .unwrap();
    if branded {
        sqlx::query(
            "INSERT INTO benefit_candidate_brands (candidate_id, brand_id) VALUES ($1, $2)",
        )
        .bind(id)
        .bind(f.brand_id)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// Test domains leave the shared test database as other suites expect it:
/// their monitors retired (global evidence counts exclude retired ones).
async fn retire(pool: &PgPool, f: &Fixture) {
    sqlx::query(
        "UPDATE program_source_monitors SET verification_state = 'retired'
         WHERE candidate_id IN (SELECT id FROM benefit_source_candidates WHERE domain_id = $1)",
    )
    .bind(f.domain_id)
    .execute(pool)
    .await
    .unwrap();
}

fn loopback(row: &CandidateRow) -> Result<Url, &'static str> {
    Url::parse(&row.url).map_err(|_| "invalid_url")
}

async fn pass(pool: &PgPool, mode: Mode, only: &[Uuid]) -> IntakeSummary {
    // Due again whatever the retry clock says (tests drive the schedule).
    sqlx::query(
        "UPDATE corporate_intake_state SET next_attempt_at = now() WHERE candidate_id = ANY($1)",
    )
    .bind(only)
    .execute(pool)
    .await
    .unwrap();
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        mode,
        false,
        &http,
        Some(only.to_vec()),
        20,
        Some(std::time::Duration::from_millis(5)),
        loopback,
        publish::PAGE_ENDING_SCAN,
    )
    .await
    .unwrap()
}

async fn last(pool: &PgPool, candidate: Uuid) -> (String, String) {
    sqlx::query_as(
        "SELECT outcome, gate FROM corporate_intake_decisions WHERE candidate_id = $1 ORDER BY decided_at DESC, id LIMIT 1",
    )
    .bind(candidate)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn backdate(pool: &PgPool, candidate: Uuid) {
    sqlx::query("UPDATE corporate_intake_offers SET first_seen_at = first_seen_at - interval '25 hours' WHERE candidate_id = $1")
        .bind(candidate)
        .execute(pool)
        .await
        .unwrap();
}

async fn programmes(pool: &PgPool, candidate: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM programs WHERE source_id = 'intake:' || $1::text")
        .bind(candidate)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn robots_allowing() -> &'static str {
    "User-agent: *\nDisallow: /private\n"
}

#[tokio::test]
async fn a_new_offer_waits_a_day_then_publishes_hidden_for_the_evidence_job() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let c = candidate(&pool, &f, &format!("{base}/education"), true).await;

    // First sighting: observed, nothing written to programmes.
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(
        last(&pool, c).await,
        ("awaiting_stability".into(), "stability".into())
    );
    assert_eq!(programmes(&pool, c).await, 0);
    // A second read within the day still waits.
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(last(&pool, c).await.0, "awaiting_stability");
    let sightings: i32 = sqlx::query_scalar("SELECT sightings FROM corporate_intake_offers WHERE candidate_id = $1 AND status = 'observed'")
        .bind(c).fetch_one(&pool).await.unwrap();
    assert_eq!(sightings, 2);

    // A day later in shadow: it would publish, and still writes no programme.
    backdate(&pool, c).await;
    pass(&pool, Mode::Shadow, &[c]).await;
    assert_eq!(
        last(&pool, c).await,
        ("would_publish".into(), "publish".into())
    );
    assert_eq!(programmes(&pool, c).await, 0);

    // Live: a hidden programme the evidence job owns.
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(last(&pool, c).await, ("published".into(), "publish".into()));
    let (program_id, active, title, summary, status): (Uuid, bool, String, String, String) = sqlx::query_as(
        "SELECT id, is_active, title, summary, program_status FROM programs WHERE source_id = 'intake:' || $1::text",
    )
    .bind(c).fetch_one(&pool).await.unwrap();
    assert!(!active, "nothing is shown before the evidence job confirms");
    assert_eq!(status, "review_required");
    assert_eq!(title, "Ableton 학생 50% 할인");
    assert!(summary.contains("50% discount"));
    let (mode, state): (String, String) = sqlx::query_as(
        "SELECT verification_mode, verification_state FROM program_source_monitors WHERE program_id = $1",
    )
    .bind(program_id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (mode.as_str(), state.as_str()),
        ("evidence_auto", "review_due")
    );
    let facts: i64 = sqlx::query_scalar("SELECT count(*) FROM corporate_offer_facts WHERE program_id = $1 AND status = 'published' AND evidence_source = 'intake'")
        .bind(program_id).fetch_one(&pool).await.unwrap();
    assert!(facts >= 1);
    let hold: String = sqlx::query_scalar("SELECT hold_reason FROM program_source_holds WHERE program_id = $1 AND resolved_at IS NULL")
        .bind(program_id).fetch_one(&pool).await.unwrap();
    assert_eq!(hold, "evidence_failed");
    let identity: i64 =
        sqlx::query_scalar("SELECT count(*) FROM corporate_offer_identities WHERE program_id = $1")
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identity, 1);
    let scheduled: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_evidence_targets WHERE candidate_id = $1",
    )
    .bind(c)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scheduled, 1, "the evidence job owns it from now on");
    let category: Option<String> = sqlx::query_scalar(
        "SELECT normalized_payload->>'category_text' FROM programs WHERE id = $1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(category.unwrap().contains("Ableton"));
    let decision_programme: Option<Uuid> = sqlx::query_scalar(
        "SELECT program_id FROM corporate_intake_decisions WHERE candidate_id = $1 ORDER BY decided_at DESC LIMIT 1",
    )
    .bind(c).fetch_one(&pool).await.unwrap();
    assert_eq!(decision_programme, Some(program_id));
    // The source is no longer a new offer: it is not selected again.
    let before = site.total();
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(site.total(), before);
    assert_eq!(programmes(&pool, c).await, 1);
    retire(&pool, &f).await;
}

#[tokio::test]
async fn duplicates_are_impossible_by_brand_offer_and_url() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    site.route("/students", 200, &offer_page(ABLETON));
    site.route("/education/", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let first = candidate(&pool, &f, &format!("{base}/education"), true).await;
    pass(&pool, Mode::Live, &[first]).await;
    backdate(&pool, first).await;
    pass(&pool, Mode::Live, &[first]).await;
    assert_eq!(last(&pool, first).await.0, "published");

    // The same brand's same offer on another page of the host.
    let same_offer = candidate(&pool, &f, &format!("{base}/students"), true).await;
    pass(&pool, Mode::Live, &[same_offer]).await;
    assert_eq!(
        last(&pool, same_offer).await,
        ("duplicate_offer".into(), "dedup".into())
    );
    // The identity row refuses even if the host check is passed: insert the
    // same key for another programme by hand.
    let key: String = sqlx::query_scalar("SELECT offer_key FROM corporate_intake_offers WHERE candidate_id = $1 AND status = 'published'")
        .bind(first).fetch_one(&pool).await.unwrap();
    let other: Uuid = sqlx::query_scalar("SELECT id FROM programs LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let conflict = sqlx::query("INSERT INTO corporate_offer_identities (brand_key, offer_key, program_id) VALUES ($1, $2, $3)")
        .bind(f.brand_id.to_string()).bind(&key).bind(other).execute(&pool).await;
    assert!(conflict.is_err(), "one programme per brand and offer");

    // Another candidate on the very URL (another canonical form).
    let same_url = candidate(&pool, &f, &format!("{base}/education/"), true).await;
    pass(&pool, Mode::Live, &[same_url]).await;
    assert_eq!(last(&pool, same_url).await.1, "dedup");
    let created: i64 = sqlx::query_scalar("SELECT count(*) FROM programs WHERE source_id IN ('intake:' || $1::text, 'intake:' || $2::text)")
        .bind(same_offer).bind(same_url).fetch_one(&pool).await.unwrap();
    assert_eq!(created, 0);
    retire(&pool, &f).await;
}

#[tokio::test]
async fn policy_gates_stop_before_any_page_request() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, "User-agent: *\nDisallow: /private\n");
    site.route("/private/offer", 200, &offer_page(ABLETON));
    site.route("/optout/offer", 200, &offer_page(ABLETON));
    site.route("/members", 200, &offer_page("<section><h2>멤버십 등급 혜택</h2><p>골드 등급 회원은 음료 10% 할인 쿠폰을 받습니다.</p></section>"));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    // robots disallows the path for our agent.
    let blocked = candidate(&pool, &f, &format!("{base}/private/offer"), true).await;
    pass(&pool, Mode::Live, &[blocked]).await;
    assert_eq!(last(&pool, blocked).await.1, "policy");
    assert_eq!(site.hits("/private/offer"), 0);
    // A publisher opt-out on the URL prefix: no request at all.
    let opted = candidate(&pool, &f, &format!("{base}/optout/offer"), true).await;
    // Loopback is plain http, so the opt-out names the host (lifted below).
    let optout: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts (domain, scope, channel, received_at, effective_at)
                 VALUES ('127.0.0.1', 'collection', 'web_form', now(), now()) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let before = site.total();
    pass(&pool, Mode::Live, &[opted]).await;
    let outcome = last(&pool, opted).await;
    sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE id = $1")
        .bind(optout)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(outcome, ("opted_out".into(), "policy".into()));
    assert_eq!(site.total(), before, "no request at all, robots included");
    // A general membership page: reader A finds no student offer.
    let junk = candidate(&pool, &f, &format!("{base}/members"), true).await;
    pass(&pool, Mode::Live, &[junk]).await;
    assert_eq!(
        last(&pool, junk).await,
        ("no_student_youth_offer".into(), "reader_a".into())
    );
    // An unbranded candidate is not a source; nothing happens.
    let unbranded = candidate(&pool, &f, &format!("{base}/members"), false).await;
    let summary = pass(&pool, Mode::Live, &[unbranded]).await;
    assert_eq!(summary.sources, 0);
    // Off: no request, no decision.
    let before = site.total();
    let http = LoopbackHttp::new(UA);
    let off = run_with(
        &pool,
        Mode::Off,
        false,
        &http,
        Some(vec![junk]),
        20,
        Some(std::time::Duration::from_millis(5)),
        loopback,
        publish::PAGE_ENDING_SCAN,
    )
    .await
    .unwrap();
    assert_eq!(off.sources, 0);
    assert_eq!(site.total(), before);
}

#[tokio::test]
async fn a_human_source_gets_facts_and_returns_through_the_publish_gate() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    site.route("/visible", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let human = |path: &'static str, held: bool| {
        let pool = pool.clone();
        let base = base.clone();
        async move {
            let c = Uuid::new_v4();
            let p = Uuid::new_v4();
            let url = format!("{base}{path}");
            sqlx::query("INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
                         VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'verification_due', 'detail')")
                .bind(c).bind(f.domain_id).bind(&url).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, official_url,
                                               program_status, provider_kind, is_active, source_health, regions)
                         VALUES ($1, 'corporate_benefit', 'corporate_official', $2, 'Ableton 학생 50% 할인', '학생 50% 할인', 'Ableton', $3,
                                 $4, 'corporate', $5, $6, ARRAY[]::text[])")
                .bind(p).bind(format!("human:{p}")).bind(&url)
                .bind(if held { "review_required" } else { "always_open" }).bind(!held)
                .bind(if held { "review_due" } else { "current" })
                .execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at, review_due_at, verification_mode)
                         VALUES ($1, $2, $3, now() - interval '60 days', now() - interval '30 days', 'human')")
                .bind(p).bind(c).bind(if held { "review_due" } else { "current" }).execute(&pool).await.unwrap();
            if held {
                sqlx::query("INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                             VALUES ($1, $2, 'verification_overdue', true, 'always_open')")
                    .bind(p).bind(c).execute(&pool).await.unwrap();
            }
            (c, p)
        }
    };
    let (held_c, held_p) = human("/education", true).await;
    pass(&pool, Mode::Live, &[held_c]).await;
    assert_eq!(last(&pool, held_c).await.0, "awaiting_stability");
    backdate(&pool, held_c).await;
    pass(&pool, Mode::Live, &[held_c]).await;
    assert_eq!(
        last(&pool, held_c).await.0,
        "published",
        "{:?}",
        last(&pool, held_c).await
    );
    let (mode, active, title): (String, bool, String) = sqlx::query_as(
        "SELECT m.verification_mode, p.is_active, p.title FROM program_source_monitors m JOIN programs p ON p.id = m.program_id WHERE p.id = $1",
    )
    .bind(held_p).fetch_one(&pool).await.unwrap();
    assert_eq!(mode, "evidence_auto");
    assert!(active);
    assert_eq!(
        title, "Ableton 학생 50% 할인",
        "composed from the facts, not the typed title"
    );
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts WHERE program_id = $1 AND status <> 'retired'",
    )
    .bind(held_p)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(facts >= 1);

    // Still visible under its person's check: nothing restores, so the
    // whole step is rolled back and the source stays human.
    let (shown_c, shown_p) = human("/visible", false).await;
    pass(&pool, Mode::Live, &[shown_c]).await;
    backdate(&pool, shown_c).await;
    pass(&pool, Mode::Live, &[shown_c]).await;
    assert_eq!(last(&pool, shown_c).await.0, "publish_refused");
    let mode: String = sqlx::query_scalar(
        "SELECT verification_mode FROM program_source_monitors WHERE program_id = $1",
    )
    .bind(shown_p)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mode, "human");
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM corporate_offer_facts WHERE program_id = $1")
            .bind(shown_p)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(facts, 0);
    retire(&pool, &f).await;
}

/// A domain paused after a bot wall or a 401/403 to our agent is a policy
/// decision, not a transient fault (production 2026-10-31: 191 candidates on
/// gong-cha, LG U+, 무신사, CGV and T world read `robots_unavailable`). The
/// source is decided `domain_paused` with no request and is retried when the
/// pause ends, not on the transient clock.
#[tokio::test]
async fn a_paused_domain_is_a_policy_decision_retried_when_the_pause_ends() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    sqlx::query("UPDATE benefit_source_domains SET next_request_at = now() + interval '5 days' WHERE id = $1")
        .bind(f.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    let c = candidate(&pool, &f, &format!("{base}/education"), true).await;
    pass(&pool, Mode::Shadow, &[c]).await;
    assert_eq!(
        last(&pool, c).await,
        ("domain_paused".into(), "policy".into())
    );
    assert_eq!(site.total(), 0, "no request while paused");
    let retry_days: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM next_attempt_at - now()) / 86400)::float8 FROM corporate_intake_state WHERE candidate_id = $1",
    )
    .bind(c)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        (4.9..=5.1).contains(&retry_days),
        "retried when the pause ends: {retry_days} days"
    );
    retire(&pool, &f).await;
}

/// A held `evidence_auto` programme the evidence job queued for fresh
/// reading is re-adopted from its own source: the whole intake chain, 24 h
/// stability, then the fresh-extraction publish, which replaces its facts
/// and restores it only if every field passes. A changed value is the same
/// case (F4): the offer is re-derived from the page as it reads now, with
/// the page's value, never confirmed from the old fact.
#[tokio::test]
async fn a_held_source_whose_quote_is_gone_is_re_adopted_through_the_publish_gate() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    site.route("/changed", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let held = |path: &'static str, outcome: &'static str| {
        let pool = pool.clone();
        let base = base.clone();
        async move {
            let c = Uuid::new_v4();
            let p = Uuid::new_v4();
            let url = format!("{base}{path}");
            sqlx::query("INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
                         VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'approved', 'detail')")
                .bind(c).bind(f.domain_id).bind(&url).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, official_url,
                                               program_status, provider_kind, is_active, source_health, regions)
                         VALUES ($1, 'corporate_benefit', 'corporate_official', $2, 'Ableton 학생 할인', '학생 할인', 'Ableton', $3,
                                 'review_required', 'corporate', false, 'review_due', ARRAY[]::text[])")
                .bind(p).bind(format!("held:{p}")).bind(&url).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at, review_due_at, verification_mode)
                         VALUES ($1, $2, 'change_detected', now() - interval '20 days', now() + interval '10 days', 'evidence_auto')")
                .bind(p).bind(c).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                         VALUES ($1, $2, 'evidence_failed', true, 'always_open')")
                .bind(p).bind(c).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO corporate_offer_facts (program_id, fact_type, value, quote, quote_key, channel, evidence_source,
                                                            pass_b, status, last_outcome, anchors)
                         VALUES ($1, 'presence', '{}'::jsonb, 'Students save 40% on Live.',
                                 encode(sha256(convert_to('old:' || $1::text, 'UTF8')), 'hex'), 'page', 'audit',
                                 '{}'::jsonb, 'hidden', $2, '{}'::jsonb)")
                .bind(p).bind(outcome).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO corporate_evidence_targets (candidate_id, reextract_requested_at, reextract_reason)
                         VALUES ($1, now(), $2)")
                .bind(c).bind(outcome).execute(&pool).await.unwrap();
            (c, p)
        }
    };
    let (gone_c, gone_p) = held("/education", "quote_missing").await;
    pass(&pool, Mode::Live, &[gone_c]).await;
    assert_eq!(last(&pool, gone_c).await.0, "awaiting_stability");
    backdate(&pool, gone_c).await;
    pass(&pool, Mode::Live, &[gone_c]).await;
    assert_eq!(
        last(&pool, gone_c).await.0,
        "published",
        "{:?}",
        last(&pool, gone_c).await
    );
    let (active, mode): (bool, String) = sqlx::query_as(
        "SELECT p.is_active, m.verification_mode FROM programs p JOIN program_source_monitors m ON m.program_id = p.id WHERE p.id = $1",
    )
    .bind(gone_p)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(active, "restored by the publish gate");
    assert_eq!(mode, "evidence_auto");
    let quotes: Vec<String> = sqlx::query_scalar(
        "SELECT quote FROM corporate_offer_facts WHERE program_id = $1 AND status <> 'retired' ORDER BY quote",
    )
    .bind(gone_p)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!quotes.is_empty());
    assert!(
        !quotes.iter().any(|q| q == "Students save 40% on Live."),
        "the gone quote is replaced: {quotes:?}"
    );

    // Held for a changed value: re-derived from the page, which states 50%.
    let (changed_c, changed_p) = held("/changed", "window_changed").await;
    pass(&pool, Mode::Live, &[changed_c]).await;
    backdate(&pool, changed_c).await;
    pass(&pool, Mode::Live, &[changed_c]).await;
    assert_eq!(last(&pool, changed_c).await.0, "published");
    let quotes: Vec<String> = sqlx::query_scalar(
        "SELECT quote FROM corporate_offer_facts WHERE program_id = $1 AND status <> 'retired'",
    )
    .bind(changed_p)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(quotes.iter().all(|q| !q.contains("40%")), "{quotes:?}");
    assert!(quotes.iter().any(|q| q.contains("50%")), "{quotes:?}");
    retire(&pool, &f).await;
}

#[tokio::test]
async fn intake_tables_are_service_only() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    for table in [
        "corporate_intake_state",
        "corporate_intake_offers",
        "corporate_offer_identities",
        "corporate_intake_decisions",
    ] {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(enabled && forced, "{table}");
    }
}

// ── the display invariant (§28): content, not layout ──────────────────────

fn plain_page(body: &str) -> String {
    format!(
        "<html><head><title>Student offer</title></head><body><nav><a href=\"/\">Home</a></nav><main>{body}<h2>About Live</h2><p>{}</p></main><footer><a href=\"/legal\">Legal</a></footer></body></html>",
        "Live is software for creating musical ideas, arranging songs and playing them on stage. ".repeat(6)
    )
}

const OFFER: &str =
    "<section><h2>Students</h2><p>Students get a 50% discount on a new Live license.</p></section>";

/// A restricting clause anywhere in the offer's section is carried word for
/// word as a note; layout (an aside, a long page, a condition far below)
/// no longer refuses.
#[test]
fn restricting_clauses_are_carried_verbatim_as_notes() {
    for (label, extra, condition) in [
        ("aside", "<aside><p>※ The student discount is for the online store only.</p></aside>", "※ The student discount is for the online store only."),
        ("far below", "<h2>Delivery</h2><p>Boxes ship in two days.</p><h2>Terms</h2><p>Limited to one new license per student.</p>", "Limited to one new license per student."),
        ("korean limit", "<ul><li>1인 1회 사용할 수 있습니다.</li></ul>", "1인 1회 사용할 수 있습니다."),
    ] {
        let checked = decide(&plain_page(&format!("{OFFER}{extra}")), "https://www.ableton.com/en/shop/education/", "Ableton")
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(checked.summary.contains("Students get a 50% discount"), "{label}: {}", checked.summary);
        assert!(checked.summary.contains(condition), "{label} lost its condition: {}", checked.summary);
    }
    // A long page is no reason to refuse.
    let long = plain_page(&format!(
        "{OFFER}<p>{}</p>",
        "Live includes instruments, effects and sounds for every kind of music. ".repeat(40)
    ));
    let checked = decide(
        &long,
        "https://www.ableton.com/en/shop/education/",
        "Ableton",
    )
    .unwrap();
    assert!(
        !checked.summary.contains("instruments, effects"),
        "only claims and conditions are shown"
    );
}

/// A restricting clause that cannot be carried word for word refuses: a
/// collapsed panel, an image's alt text, a tooltip, a footnote mark whose
/// footnote is missing.
#[test]
fn a_condition_that_cannot_be_carried_refuses() {
    for (label, extra) in [
        ("collapsed", "<div><button aria-expanded=\"false\">Terms</button><div style=\"display:none\"><p>Not valid on upgrades.</p></div></div>"),
        ("alt", "<p><img src=\"/n.png\" alt=\"Not valid on upgrades\"></p>"),
        ("tooltip", "<p>Details <span title=\"Excluding upgrades\">i</span></p>"),
    ] {
        let result = decide(&plain_page(&format!("{OFFER}{extra}")), "https://www.ableton.com/en/shop/education/", "Ableton");
        assert!(result.is_err(), "{label} must refuse: {:?}", result.map(|c| c.summary));
    }
    let marked = plain_page("<section><h2>Students</h2><p>Students get a 50% discount on a new Live license.*</p></section>");
    assert!(
        decide(
            &marked,
            "https://www.ableton.com/en/shop/education/",
            "Ableton"
        )
        .is_err(),
        "footnote without its text"
    );
}

// ── link discovery ────────────────────────────────────────────────────────

#[test]
fn an_offer_page_is_recognised_by_its_own_content() {
    let offer = "<html><head><title>Ableton Live for students and teachers</title></head><body><h1>Live 12</h1></body></html>";
    assert_eq!(
        offer_page_signal(offer).as_deref(),
        Some("Ableton Live for students and teachers")
    );
    let heading =
        "<html><head><title>Shop</title></head><body><h2>대학생 10% 할인 혜택</h2></body></html>";
    assert_eq!(
        offer_page_signal(heading).as_deref(),
        Some("대학생 10% 할인 혜택")
    );
    // A membership page, a page that only links to students, a book title.
    for html in [
        "<html><head><title>멤버십 등급 혜택</title></head><body><h1>골드 회원 할인</h1></body></html>",
        "<html><head><title>Shop</title></head><body><nav><a href=\"/s\">Students discount</a></nav><h1>All products</h1></body></html>",
        "<html><head><title>대학생을 위한 글쓰기 - 도서</title></head><body><h1>도서 상세</h1></body></html>",
    ] {
        assert_eq!(offer_page_signal(html), None, "{html}");
    }
}

#[test]
fn links_stay_on_the_host_and_prefer_offer_words() {
    let page = Url::parse("https://www.ableton.com/en/shop/").unwrap();
    let html = r##"<a href="/en/about/">About</a><a href="https://other.example/x">Other</a>
        <a href="/en/shop/education/#top">Students and teachers save</a><a href="/en/login">Log in</a>
        <a href="/en/cart">Cart</a><a href="/files/price.pdf">Price list</a><a href="mailto:a@b.c">Mail</a>
        <a href="http://www.ableton.com/plain">Plain http</a><a href="/en/about/">About again</a>"##;
    let links: Vec<String> = same_host_links(html, &page)
        .iter()
        .map(Url::to_string)
        .collect();
    assert_eq!(
        links,
        vec![
            "https://www.ableton.com/en/shop/education/",
            "https://www.ableton.com/en/about/"
        ]
    );
}

#[tokio::test]
async fn discovery_follows_allowed_links_to_offer_pages_within_budget() {
    let site = Site::default();
    site.route("/robots.txt", 200, "User-agent: *\nDisallow: /private\n");
    site.route(
        "/",
        200,
        "<html><head><title>Ableton</title></head><body><a href=\"/shop\">Shop</a><a href=\"/private/students\">Students</a><a href=\"/about\">About</a></body></html>",
    );
    site.route(
        "/shop",
        200,
        "<html><head><title>Shop</title></head><body><a href=\"/shop/education\">Education</a><a href=\"/shop/known\">Known</a></body></html>",
    );
    let titled = offer_page(ABLETON).replace(
        "<title>Education</title>",
        "<title>Ableton Live for students and teachers</title>",
    );
    site.route("/shop/education", 200, &titled);
    site.route("/shop/known", 200, &titled);
    site.route("/private/students", 200, &titled);
    site.route("/about", 200, "<html><head><title>About us</title></head><body><h1>Our story</h1><a href=\"/about/team\">Team</a></body></html>");
    site.route("/about/team", 200, "<html><head><title>Team</title></head><body><a href=\"/about/team/deep\">Deep</a></body></html>");
    site.route("/about/team/deep", 200, &titled);
    let base = serve(site.clone()).await;
    let http = LoopbackHttp::new(UA);
    let mut session = DomainSession::new(&http, None, None, true, 0, 50)
        .with_test_spacing(std::time::Duration::from_millis(5));
    let seed = Url::parse(&format!("{base}/")).unwrap();
    let known: std::collections::HashSet<String> = [url_key(&format!("{base}/shop/known"))].into();
    let found = discover_from(&mut session, &[seed], &known, true, None).await;
    let urls: Vec<String> = found.iter().map(|f| f.url.clone()).collect();
    assert_eq!(urls, vec![format!("{base}/shop/education")], "{found:?}");
    assert_eq!(found[0].depth, 2);
    assert_eq!(
        site.hits("/private/students"),
        0,
        "robots holds on every link"
    );
    assert_eq!(
        site.hits("/about/team/deep"),
        0,
        "deeper than the depth limit"
    );
    assert_eq!(
        site.hits("/shop/known"),
        0,
        "a known candidate is not fetched again"
    );

    // The budget: one page request in all.
    let mut tight = DomainSession::new(&http, None, None, true, 0, 2)
        .with_test_spacing(std::time::Duration::from_millis(5));
    let seed = Url::parse(&format!("{base}/")).unwrap();
    let before = site.total();
    let found = discover_from(&mut tight, &[seed], &Default::default(), true, None).await;
    assert!(found.is_empty());
    assert!(site.total() - before <= 2, "robots plus one page");
}

#[tokio::test]
async fn discovery_queues_found_offer_pages_as_branded_review_candidates() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    let titled = offer_page(ABLETON).replace(
        "<title>Education</title>",
        "<title>Ableton Live for students and teachers</title>",
    );
    site.route("/shop", 200, "<html><head><title>Shop</title></head><body><a href=\"/shop/education\">Students and teachers</a><a href=\"/shop/live\">Live</a></body></html>");
    site.route("/shop/education", 200, &titled);
    site.route(
        "/shop/live",
        200,
        "<html><head><title>Live 12</title></head><body><h1>Live</h1></body></html>",
    );
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let seed = candidate(&pool, &f, &format!("{base}/shop"), true).await;
    let http = LoopbackHttp::new(UA);
    let summary = discover_with(
        &pool,
        Mode::Shadow,
        &http,
        Some(vec![f.domain_id]),
        Some(std::time::Duration::from_millis(5)),
        loopback,
    )
    .await
    .unwrap();
    assert_eq!(summary.found, 1, "{summary:?}");
    let (status, via, brands): (String, String, i64) = sqlx::query_as(
        "SELECT c.status, c.discovered_via, (SELECT count(*) FROM benefit_candidate_brands b WHERE b.candidate_id = c.id)
         FROM benefit_source_candidates c WHERE c.url = $1",
    )
    .bind(format!("{base}/shop/education"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (status.as_str(), via.as_str(), brands),
        ("review", "official_page", 1)
    );
    let found = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM corporate_intake_decisions WHERE outcome = 'discovered' AND candidate_id IN (SELECT id FROM benefit_source_candidates WHERE domain_id = $1)")
        .bind(f.domain_id).fetch_one(&pool).await.unwrap();
    assert_eq!(found, 1, "one audit row per found page");
    // Not explored again within the week; the found page is now known.
    let before = site.total();
    let again = discover_with(
        &pool,
        Mode::Shadow,
        &http,
        Some(vec![f.domain_id]),
        Some(std::time::Duration::from_millis(5)),
        loopback,
    )
    .await
    .unwrap();
    assert_eq!((again.domains, site.total()), (0, before));
    // Off: nothing.
    sqlx::query("UPDATE corporate_discovery_state SET next_run_at = now() WHERE domain_id = $1")
        .bind(f.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    let off = discover_with(
        &pool,
        Mode::Off,
        &http,
        Some(vec![f.domain_id]),
        Some(std::time::Duration::from_millis(5)),
        loopback,
    )
    .await
    .unwrap();
    assert_eq!((off.domains, site.total()), (0, before));
    let _ = seed;
}

// ── precision classes (2026-10-31 hand check of 23 accepts) ───────────────
//
// Every case is a real page's sentence (CorelDRAW, Capture One, Kotlin,
// Imgbot, InterviewCake, DeepScan, Honeybadger, GoRails, CodeScene,
// Headspace, Amtrak, Wacom), or the same class in Korean wording.

fn refused(html: &str, url: &str, brand: &str) -> String {
    match decide(html, url, brand) {
        Ok(checked) => panic!("{brand} accepted: {} / {}", checked.title, checked.summary),
        Err(reason) => reason,
    }
}

/// Rule 1: a value counts only for the audience nearest to it in its own
/// sentence; an institution or teachers nearer to it void it.
#[test]
fn a_value_belongs_to_the_audience_nearest_to_it() {
    let corel = offer_page(
        "<section><h2>Students and teachers</h2><p>with flexible pricing options for students, teachers, and academic institutions. Schools can save up to 75% off commercial pricing with special discounts.</p></section>",
    );
    assert!(refused(
        &corel,
        "https://www.coreldraw.com/en/pages/education/",
        "CorelDRAW"
    )
    .starts_with("reader_a:"));
    let capture = offer_page(
        "<section><h3>For students</h3><p>Verify and get 75% off Capture One</p><h3>For teachers</h3><p>Verify and get 65% off Capture One</p></section>",
    );
    let checked = decide(
        &capture,
        "https://www.captureone.com/en/education",
        "Capture One",
    )
    .unwrap();
    assert_eq!(checked.title, "Capture One 학생 75% 할인");
    assert!(!checked.summary.contains("65%"), "{}", checked.summary);
    let school = offer_page(
        "<section><h2>교육용 라이선스</h2><p>학생과 교사 전용 교육용 라이선스를 제공합니다. 학교·기관은 최대 60% 할인된 가격으로 구매할 수 있습니다.</p></section>",
    );
    assert!(refused(&school, "https://example.co.kr/edu", "예시").starts_with("reader_a:"));
    let staff = offer_page(
        "<section><h2>교육 할인</h2><p>대학생은 학생증 인증 후 20% 할인됩니다. 교직원은 10% 할인됩니다.</p></section>",
    );
    let checked = decide(&staff, "https://example.co.kr/edu", "예시").unwrap();
    assert_eq!(checked.title, "예시 대학생 20% 할인");
    assert!(!checked.summary.contains("10%"), "{}", checked.summary);
}

/// Rule 2: a link label and a sentence whose subject is a verification
/// programme or a partner company state someone else's offer.
#[test]
fn someone_elses_offer_is_not_the_pages_offer() {
    let kotlin = offer_page(
        "<section><h2>Teaching Kotlin</h2><p><a href=\"https://www.jetbrains.com/community/education/\">Free educational licenses for students and teachers</a></p></section>",
    );
    assert!(
        refused(&kotlin, "https://kotlinlang.org/education/", "Kotlin").starts_with("reader_a:")
    );
    let pack = offer_page(
        "<section><h2>What is the GitHub Student Developer Pack?</h2><p>The GitHub Student Developer Pack is a program created by GitHub that helps students learn through doing by giving them free access to the best developer tools in one place.</p></section>",
    );
    assert!(
        refused(&pack, "https://imgbot.net/github-students", "Imgbot").starts_with("reader_a:")
    );
    let link = offer_page(
        "<section><h2>제휴 혜택</h2><p><a href=\"/partner/card\">제휴 카드 대학생 50% 할인 혜택 보기</a></p></section>",
    );
    assert!(refused(&link, "https://example.co.kr/benefit", "예시").starts_with("reader_a:"));
    let partner = offer_page(
        "<section><h2>제휴 혜택</h2><p>제휴사는 대학생 회원에게 30% 할인 쿠폰을 제공합니다.</p></section>",
    );
    assert!(refused(&partner, "https://example.co.kr/benefit", "예시").starts_with("reader_a:"));
    // The owner's own sentence next to the programme stays the offer.
    let imgbot = offer_page(
        "<section><h1>Free image optimization for students</h1><p>Don't spend another second worrying about compressing your images.</p></section>",
    );
    let checked = decide(&imgbot, "https://imgbot.net/github-students", "Imgbot").unwrap();
    assert!(
        checked
            .summary
            .contains("Free image optimization for students"),
        "{}",
        checked.summary
    );
}

/// Rule 3: a free plan offered to everyone is no student offer; the free
/// claim must be bound to students by rule 1.
#[test]
fn a_free_plan_for_everyone_is_not_a_student_offer() {
    let english = offer_page(
        "<section><h2>Pricing</h2><p>Our student discount program is open. Everyone can use the Free plan at no cost.</p></section>",
    );
    assert!(refused(&english, "https://example.com/pricing", "Example").starts_with("reader_a:"));
    let korean = offer_page(
        "<section><h2>요금제</h2><p>대학생 할인 프로그램을 운영합니다. 누구나 무료 플랜을 이용할 수 있습니다.</p></section>",
    );
    assert!(refused(&korean, "https://example.co.kr/pricing", "예시").starts_with("reader_a:"));
}

/// Rule 4: a free offer carries the one free period the page states for
/// it and shows it; two periods refuse; a trial is shown as one, and a
/// period under a month is no offer.
#[test]
fn a_free_offer_carries_and_shows_its_term() {
    let honeybadger = offer_page(
        "<section><h1>Free Honeybadger for GitHub students</h1><p>Honeybadger is stoked to be part of GitHub Student Developer Pack!</p><p>Level-up your software development career and get access to the error monitoring solution used by the pros free for an entire year (over a $700 value).</p></section>",
    );
    let checked = decide(
        &honeybadger,
        "https://www.honeybadger.io/github-students/",
        "Honeybadger",
    )
    .unwrap();
    assert_eq!(checked.title, "Honeybadger 학생 1년 무료");
    let deepscan = offer_page(
        "<section><h1>DeepScan for GitHub Students</h1><p>A free offer for students enrolled in the GitHub Student Developer Pack</p><p>As part of the GitHub Student Developer Pack, DeepScan gives a free 6-month trial for GitHub Students.</p></section>",
    );
    let checked = decide(
        &deepscan,
        "https://deepscan.io/github-student-pack",
        "DeepScan",
    )
    .unwrap();
    assert_eq!(checked.title, "DeepScan 학생 6개월 무료 체험");
    assert!(
        checked
            .summary
            .starts_with("학생은 6개월 동안 무료로 체험할 수 있습니다."),
        "{}",
        checked.summary
    );
    let interviewcake = offer_page(
        "<section><h2>Free Coding Interview Prep for Students</h2><p>As part of the GitHub Student Developer Pack</p><p>Students get access to <a href=\"/table-of-contents\">our full coding interview prep course</a> for 1 week for FREE.</p></section>",
    );
    assert_eq!(
        refused(
            &interviewcake,
            "https://www.interviewcake.com/github-students",
            "InterviewCake"
        ),
        "reader_a:short_free_term"
    );
    let gorails = offer_page(
        "<section><h2>GoRails for GitHub Students</h2><p>3 months of free access to GoRails for students</p><p>As a GitHub Student Developer, you can get a 12 month subscription to GoRails for free!</p></section>",
    );
    assert_eq!(
        refused(&gorails, "https://gorails.com/github-students", "GoRails"),
        "reader_a:conflicting_terms"
    );
    let year = offer_page(
        "<section><h2>대학생 혜택</h2><p>대학생은 인증 후 무료로 이용할 수 있습니다.</p><p>인증 완료 시 1년간 무료로 제공됩니다.</p></section>",
    );
    assert_eq!(
        decide(&year, "https://example.co.kr/edu", "예시")
            .unwrap()
            .title,
        "예시 대학생 1년 무료"
    );
    let trial = offer_page(
        "<section><h2>대학생 혜택</h2><p>대학생은 3개월 무료 체험판을 받을 수 있습니다.</p></section>",
    );
    assert_eq!(
        decide(&trial, "https://example.co.kr/edu", "예시")
            .unwrap()
            .title,
        "예시 대학생 3개월 무료 체험"
    );
    let week = offer_page(
        "<section><h2>대학생 혜택</h2><p>대학생 회원은 7일 무료 체험을 이용할 수 있습니다.</p></section>",
    );
    assert!(refused(&week, "https://example.co.kr/edu", "예시").starts_with("reader_a:"));
    let six = offer_page(
        "<section><h2>청년 요금제</h2><p>청년 고객은 6개월 무료로 이용하세요.</p></section>",
    );
    assert_eq!(
        decide(&six, "https://example.co.kr/young", "예시")
            .unwrap()
            .title,
        "예시 청년 6개월 무료"
    );
}

/// Rule 5: a page that states when it was published, with no own date in
/// the last year, is stale; a maintained page's old edit date alone is not.
#[test]
fn a_page_published_over_a_year_ago_is_stale() {
    let body = "<p>This means that CodeScene is available for free to every student developer.</p>";
    let codescene = offer_page(&format!(
        "<article><div class=\"meta-time-mobile\"><p class=\"csb-post2--blog--author__display__date\">\n  Jan 19, 2020\n </p></div>{body}</article>"
    ));
    let url = "https://codescene.com/blog/codescene-in-github-student-developer-pack";
    assert_eq!(refused(&codescene, url, "CodeScene"), "reader_a:stale_page");
    let ld = |published: &str, modified: &str| {
        offer_page(&format!(
            "<script type=\"application/ld+json\">{{\"@type\":\"BlogPosting\",\"datePublished\":\"{published}\",\"dateModified\":\"{modified}\"}}</script>{body}"
        ))
    };
    assert_eq!(
        refused(
            &ld("2017-12-14T00:00:00+01:00", "2019-05-21"),
            url,
            "CodeScene"
        ),
        "reader_a:stale_page"
    );
    assert!(decide(
        &ld("2017-12-14T00:00:00+01:00", "2026-05-21T00:00:00+01:00"),
        url,
        "CodeScene"
    )
    .is_ok());
    // A web page's own CMS dates (Axure's education page) date no article.
    let webpage = offer_page(&format!(
        "<script type=\"application/ld+json\">{{\"@type\":\"WebPage\",\"datePublished\":\"2025-06-12\"}}</script>{body}"
    ));
    assert!(decide(&webpage, url, "CodeScene").is_ok());
    let edited = offer_page(&format!(
        "<meta property=\"article:modified_time\" content=\"2024-07-08T14:28:59+00:00\">{body}"
    ));
    assert!(decide(&edited, url, "CodeScene").is_ok());
    let korean = |date: &str| {
        offer_page(&format!(
            "<section><h2>공지</h2><p>{date}</p><p>대학생 인증 시 15% 할인 혜택을 드립니다.</p></section>"
        ))
    };
    assert_eq!(
        refused(
            &korean("게시일 2023.05.01"),
            "https://example.co.kr/notice/1",
            "예시"
        ),
        "reader_a:stale_page"
    );
    assert!(decide(
        &korean("등록일: 2026-09-01"),
        "https://example.co.kr/notice/1",
        "예시"
    )
    .is_ok());
}

/// Rule 6: an offer the page restricts to another country, or a store of
/// another country, is out of scope; a global offer on an en-us page stays.
#[test]
fn an_offer_restricted_to_another_country_is_out_of_scope() {
    let cases = [
        (
            "Headspace",
            "https://www.headspace.com/studentplan",
            "<section><h2>Student Plan</h2><p>85% off, just for students</p><p>Currently available for students within the United States, UK, Australia, Canada, France, Portugal, and Spain attending a post-secondary program or university, aged 18+.</p></section>",
        ),
        (
            "CorelDRAW",
            "https://www.coreldraw.com/en/pages/education/",
            "<section><h2>Students and teachers</h2><p>Students save 50% on CorelDRAW Graphics Suite.</p><p>Product available in the United States and Canada. Validation required.</p></section>",
        ),
        (
            "Amtrak",
            "https://www.amtrak.com/student-discount",
            "<section><h3>Students Save 15%</h3><p>Students 17 - 24 save 15% to top destinations across America.</p></section>",
        ),
        (
            "Wacom",
            "https://www.wacom.com/en-us/education",
            "<section><h2>Education discount</h2><p>Save up to 10% on the Wacom eStore as student or teacher. Available on selected Wacom products.</p></section>",
        ),
        (
            "예시",
            "https://example.co.kr/edu",
            "<section><h2>대학생 할인</h2><p>대학생은 미국 스토어에서 10% 할인을 받을 수 있습니다.</p></section>",
        ),
    ];
    for (brand, url, body) in cases {
        let reason = refused(&offer_page(body), url, brand);
        assert!(
            reason.starts_with("scope:region_out_of_scope"),
            "{brand} {body}: {reason}"
        );
    }
    // Korea excluded in so many words (`국내 사용이 불가합니다`, `not offered
    // in Korea`) is refused before the region check, by the section's
    // negation scan.
    let not_here = offer_page(
        "<section><h2>Student discount</h2><p>Students save 20% on every plan.</p><p>This student offer is not offered in Korea.</p></section>",
    );
    refused(&not_here, "https://example.com/students", "Example");
    let excluded = offer_page(
        "<section><h2>대학생 할인</h2><p>대학생 20% 할인 혜택을 드립니다.</p><p>해외 결제 상품으로 국내 사용이 불가합니다.</p></section>",
    );
    refused(&excluded, "https://example.co.kr/edu", "예시");
    let azure = offer_page(
        "<section><h2>Azure for Students</h2><p>Students can use popular Azure services free for 12 months.</p></section>",
    );
    assert!(decide(
        &azure,
        "https://azure.microsoft.com/en-us/free/students",
        "Azure"
    )
    .is_ok());
    let domestic = offer_page(
        "<section><h2>대학생 할인</h2><p>대학생은 국내 공식 스토어에서 10% 할인을 받을 수 있습니다.</p></section>",
    );
    assert!(decide(&domestic, "https://example.co.kr/edu", "예시").is_ok());
}

/// Robots that never answer (25 domestic domains: no reply on robots.txt)
/// stay transient, then become a weekly policy state once the failures span
/// separate days; the page is never requested either way.
#[test]
fn robots_that_never_answer_become_a_policy_state() {
    let first = escalate("transient", "robots_unavailable".into(), 1);
    assert_eq!(first, ("transient", "robots_unavailable".to_string()));
    assert_eq!(retry_after(first.0, 1), TRANSIENT_RETRY);
    // 6 h, 12 h, 24 h: the fourth failure is over a day after the first.
    let span: Duration = (1..ROBOTS_SILENT_AFTER)
        .map(|n| retry_after("transient", n))
        .sum();
    assert!(span > Duration::days(1), "{span}");
    let silent = escalate(
        "transient",
        "robots_unavailable".into(),
        ROBOTS_SILENT_AFTER,
    );
    assert_eq!(silent, ("policy", ROBOTS_SILENT.to_string()));
    assert_eq!(retry_after(silent.0, ROBOTS_SILENT_AFTER), REFRESH_AFTER);
    // Other transient failures keep backing off as before.
    assert_eq!(
        escalate("transient", "page_unreachable".into(), 9),
        ("transient", "page_unreachable".to_string())
    );
}

/// Robots counts only its own streak (review L1): page failures before a
/// robots failure do not bring the robots policy state early, and a
/// robots.txt that redirects off the host or in a loop is named as such,
/// not folded into `robots_unavailable`.
#[test]
fn a_robots_streak_counts_only_robots_failures() {
    // Five unreachable pages, then robots fails once: a streak of one.
    let streak = failure_streak(Some("page_unreachable"), "robots_unavailable", 5, true);
    assert_eq!(streak, 1);
    assert_eq!(
        escalate("transient", "robots_unavailable".into(), streak),
        ("transient", "robots_unavailable".to_string())
    );
    // The same failure again extends the streak; a decision resets it.
    assert_eq!(
        failure_streak(Some("robots_unavailable"), "robots_unavailable", 3, true),
        4
    );
    assert_eq!(
        failure_streak(Some(ROBOTS_SILENT), "robots_unavailable", 4, true),
        5
    );
    assert_eq!(
        failure_streak(
            Some("robots_unavailable"),
            "no_student_youth_offer",
            3,
            false
        ),
        0
    );
    for reason in ["robots_redirect_off_host", "robots_redirect_loop"] {
        assert_eq!(gate_of(reason), "policy", "{reason}");
    }
}

/// Rule 7: a free offer is shown with its term or not at all. A period
/// next to it that names no student (nc.me's bundle line `+1 year pages
/// setup`) leaves the term unknown; and an unrendered
/// template (adobe.com: `Free for eligible students and teachers. Not
/// included in {{creative-cloud-pro-geo-ip}}.`) is not read.
#[test]
fn a_free_offer_with_an_unbound_term_or_an_unfilled_template_is_refused() {
    let miro = offer_page(
        "<section><h2>Free digital whiteboard for online education</h2><table><tr><th>Students and educators</th></tr><tr><td>Free</td></tr><tr><td>2 years</td></tr></table></section>",
    );
    // Read as one cell under the other (`Free` + `2 years`), the term binds.
    assert_eq!(
        decide(&miro, "https://miro.com/education-whiteboard/", "Miro")
            .unwrap()
            .title,
        "Miro 학생 2년 무료"
    );
    let nc = offer_page(
        "<section><h2>Claim your free domain</h2><p>Namecheap is giving university students a free bundle</p><ul><li>+1 year pages setup</li></ul></section>",
    );
    assert_eq!(
        refused(&nc, "https://nc.me/", "Namecheap"),
        "reader_a:unbound_term"
    );
    let korean = offer_page(
        "<section><h2>대학생 혜택</h2><p>대학생은 프리미엄 요금제를 무료로 이용할 수 있습니다.</p><p>이용 기간 1년</p></section>",
    );
    assert_eq!(
        refused(&korean, "https://example.co.kr/edu", "예시"),
        "reader_a:unbound_term"
    );
    let adobe = offer_page(
        "<section><h3>{{substance-3d-collection}}</h3><p>Model, texture, and render 3D assets and scenes. Free for eligible students and teachers. Not included in {{creative-cloud-pro-geo-ip}}.</p></section>",
    );
    assert_eq!(
        refused(
            &adobe,
            "https://www.adobe.com/creativecloud/buy/students.html",
            "Adobe"
        ),
        "reader_a:template_shell"
    );
    // A student-bound period next to the free claim is its term (axure.com:
    // `Students: … request a one-year Axure RP subscription`), and a
    // feature's days are no term (notion.com: `30-day page history`).
    let axure = offer_page(
        "<section><h1>Free Subscriptions for Students and Teachers</h1><p>Students: Click the button below to request a one-year Axure RP subscription each year you are enrolled in school.</p></section>",
    );
    assert_eq!(
        decide(&axure, "https://www.axure.com/edu", "Axure")
            .unwrap()
            .title,
        "Axure 학생 1년 무료"
    );
    let notion = offer_page(
        "<section><h2>Run your student org, for free</h2><p>With the Education Plan, student orgs can upload unlimited blocks, get 30-day page history, and more.</p></section>",
    );
    assert_eq!(
        decide(
            &notion,
            "https://www.notion.com/product/notion-for-education",
            "Notion"
        )
        .unwrap()
        .title,
        "Notion 학생 무료"
    );
    // A card's term a few lines below its free cell (miro.com) is next to it.
    let card = offer_page(
        "<section><h3>Students</h3><p>Work on assignments and group projects</p><p>Free</p><p><a href=\"/apply\">Apply as student</a></p><p>What's included:</p><ul><li>10 team members</li><li>2 years</li><li>Unlimited boards</li></ul></section>",
    );
    assert_eq!(
        refused(&card, "https://miro.com/education-whiteboard/", "Miro"),
        "reader_a:unbound_term"
    );
    // A free offer whose page states no period stays.
    let todiagram =
        offer_page("<section><h2>Education</h2><p>ToDiagram is free for students</p></section>");
    assert!(decide(&todiagram, "https://todiagram.com/education", "ToDiagram").is_ok());
}

/// A free claim about verifying with a verification programme is that
/// programme's step, not the offer, and a bare free claim never outranks a
/// stated value (ko.squarespace.com/students, 2026-10-31: the offer is `첫해
/// 50%* 할인`; `Student Beans에 학생 신분을 인증하면 … 무료입니다!` is the
/// verifier's).
#[test]
fn a_verifiers_free_step_and_a_bare_free_claim_do_not_make_the_offer() {
    let squarespace = offer_page(
        "<section><h2>학생 할인</h2><p>Squarespace 첫해에는 학생들에게 50%* 할인 혜택을 제공하고 있습니다.</p><p>파트너사인 Student Beans에 학생 신분을 인증하면 할인 혜택을 받을 수 있습니다. 무료입니다! Squarespace.com에서 결제 시 할인 코드를 입력하세요.</p><p>*Squarespace 웹사이트의 새로운 연간 구독 요금제 구매 시 할인이 적용됩니다.</p></section>",
    );
    let checked = decide(
        &squarespace,
        "https://ko.squarespace.com/students",
        "Squarespace",
    )
    .unwrap();
    assert_eq!(checked.title, "Squarespace 학생 50% 할인");
    let verifier_only = offer_page(
        "<section><h2>학생 인증</h2><p>Student Beans에서 학생 인증은 무료입니다.</p></section>",
    );
    assert!(refused(
        &verifier_only,
        "https://ko.squarespace.com/students",
        "Squarespace"
    )
    .starts_with("reader_a:"));
    let english = offer_page(
        "<section><h2>Students</h2><p>Verify your student status with Student Beans for free.</p></section>",
    );
    assert!(refused(
        &english,
        "https://www.squarespace.com/students",
        "Squarespace"
    )
    .starts_with("reader_a:"));
    let won = offer_page(
        "<section><h2>대학생 혜택</h2><p>대학생은 3,000원 할인을 받을 수 있습니다.</p><p>대학생 회원 혜택 안내는 무료로 받아보세요.</p></section>",
    );
    assert_eq!(
        decide(&won, "https://example.co.kr/edu", "예시")
            .unwrap()
            .title,
        "예시 대학생 3,000원 할인"
    );
}

// ── seeded scope: live only on reviewed seed URLs ────────────────────────

/// Registers `url` as a reviewed seed URL of the fixture's brand: an active
/// discovery target, its official domain verified and the fixture's domain,
/// linked to the brand. `status` and `brand` vary it for the negative cases.
async fn seed_target(pool: &PgPool, f: &Fixture, url: &str, status: &str, brand: Uuid) {
    let domain: String =
        sqlx::query_scalar("SELECT domain FROM benefit_source_domains WHERE id = $1")
            .bind(f.domain_id)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO brand_discovery_targets (target_key, brand_id, display_name, coverage_category,
                                              official_domain, official_homepage_url, domain_status, seed_urls)
         VALUES ('seed_test_' || $1::text, $2, 'Ableton', '전자·소프트웨어', $3, 'https://' || $3 || '/', $4, ARRAY[$5])",
    )
    .bind(Uuid::new_v4())
    .bind(brand)
    .bind(&domain)
    .bind(status)
    .bind(url)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO benefit_source_domain_brands (domain_id, brand_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(f.domain_id)
    .bind(f.brand_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn drop_seed_targets(pool: &PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM brand_discovery_targets WHERE target_key LIKE 'seed_test_%' AND (brand_id = $1 OR display_name = 'Ableton')")
        .bind(f.brand_id)
        .execute(pool)
        .await
        .unwrap();
}

/// `seeded` publishes the stable, accepted offer of a reviewed seed URL,
/// with its post-check recorded, and keeps every other source in shadow:
/// another page of the same verified domain, a seed whose domain is not
/// verified, and a seed registered for another brand.
#[tokio::test]
async fn seeded_publishes_only_from_reviewed_seed_urls() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    site.route("/students", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let other_brand: Uuid = sqlx::query_scalar(
        "INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
         VALUES ('Other', $1, 'intake_test', $1, 50) RETURNING id",
    )
    .bind(format!("other{}", f.domain_id.simple()))
    .fetch_one(&pool)
    .await
    .unwrap();

    let seeded = candidate(&pool, &f, &format!("{base}/education"), true).await;
    let discovered = candidate(&pool, &f, &format!("{base}/students"), true).await;
    seed_target(
        &pool,
        &f,
        &format!("{base}/education"),
        "verified",
        f.brand_id,
    )
    .await;
    // The discovered page first: once the seed publishes, the same offer
    // of the brand is a duplicate anywhere else.
    for c in [discovered, seeded] {
        pass(&pool, Mode::Seeded, &[c]).await;
        backdate(&pool, c).await;
        pass(&pool, Mode::Seeded, &[c]).await;
    }
    assert_eq!(
        last(&pool, seeded).await,
        ("published".into(), "publish".into())
    );
    assert_eq!(programmes(&pool, seeded).await, 1);
    let (mode, detail): (String, String) = sqlx::query_as(
        "SELECT mode, detail FROM corporate_intake_decisions WHERE candidate_id = $1 ORDER BY decided_at DESC, id LIMIT 1",
    )
    .bind(seeded)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mode, "live");
    assert!(
        detail.contains("post-check: quotes")
            && detail.contains("audience")
            && detail.contains("window"),
        "{detail}"
    );
    assert_eq!(
        last(&pool, discovered).await,
        ("would_publish".into(), "publish".into()),
        "a page found on the same domain is no seed"
    );
    assert_eq!(programmes(&pool, discovered).await, 0);
    retire(&pool, &f).await;
    drop_seed_targets(&pool, &f).await;

    // A seed whose domain is only a candidate, or that names another
    // brand, stays shadow.
    for (status, brand) in [("candidate", f.brand_id), ("verified", other_brand)] {
        // Its own site: the first one's URL is already published.
        let site = Site::default();
        site.route("/robots.txt", 200, robots_allowing());
        site.route("/education", 200, &offer_page(ABLETON));
        let base = serve(site.clone()).await;
        let f = domain(&pool).await;
        let c = candidate(&pool, &f, &format!("{base}/education"), true).await;
        seed_target(&pool, &f, &format!("{base}/education"), status, brand).await;
        pass(&pool, Mode::Seeded, &[c]).await;
        backdate(&pool, c).await;
        pass(&pool, Mode::Seeded, &[c]).await;
        assert_eq!(last(&pool, c).await.0, "would_publish", "{status}");
        assert_eq!(programmes(&pool, c).await, 0, "{status}");
        retire(&pool, &f).await;
        drop_seed_targets(&pool, &f).await;
    }
}

/// The recorded post-check: every quote verbatim on the page just read, an
/// audience, a value and a window still open; each missing part refuses.
#[test]
fn the_post_check_names_what_a_seeded_publish_rests_on() {
    let html = offer_page(ABLETON);
    let checked = decide(&html, "https://www.ableton.com/education", "Ableton").unwrap();
    let text = super::evidence::visible_text(&html);
    let now = Utc::now();
    let record = post_check(&checked, &text, now).unwrap();
    assert!(record.starts_with("post-check: quotes"), "{record}");
    assert!(
        record.contains("audience") && record.contains("value") && record.contains("window open"),
        "{record}"
    );
    // The quote is no longer on the page.
    let gone = super::evidence::visible_text(&offer_page("<p>Our store.</p>"));
    assert!(post_check(&checked, &gone, now)
        .unwrap_err()
        .starts_with("quote:"));
    // The window closed.
    let mut ended = checked.clone();
    ended.valid_until = Some(now - Duration::days(1));
    assert!(post_check(&ended, &text, now)
        .unwrap_err()
        .starts_with("window:"));
    // No audience read.
    let mut nobody = checked.clone();
    nobody.offer.audience.clear();
    assert!(post_check(&nobody, &text, now)
        .unwrap_err()
        .starts_with("audience:"));
}

/// A held offer whose section now says something new is re-read through
/// the whole chain from the page as it reads now (never confirmed from the
/// old fact), in `live` and in `seeded` (it was published from this URL
/// before). A fact held for an ending is not a source.
#[tokio::test]
async fn a_held_source_whose_section_changed_is_re_read_through_the_chain() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    for path in ["/live", "/seeded", "/ended"] {
        site.route(path, 200, &offer_page(ABLETON));
    }
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let held = |path: &'static str, reason: &'static str| {
        let pool = pool.clone();
        let base = base.clone();
        async move {
            let c = Uuid::new_v4();
            let p = Uuid::new_v4();
            let url = format!("{base}{path}");
            sqlx::query("INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
                         VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'approved', 'detail')")
                .bind(c).bind(f.domain_id).bind(&url).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, official_url,
                                               program_status, provider_kind, is_active, source_health, regions)
                         VALUES ($1, 'corporate_benefit', 'corporate_official', $2, 'Ableton 학생 할인', '학생 할인', 'Ableton', $3,
                                 'review_required', 'corporate', false, 'review_due', ARRAY[]::text[])")
                .bind(p).bind(format!("held:{p}")).bind(&url).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at, review_due_at, verification_mode)
                         VALUES ($1, $2, 'change_detected', now() - interval '20 days', now() + interval '10 days', 'evidence_auto')")
                .bind(p).bind(c).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                         VALUES ($1, $2, 'evidence_failed', true, 'always_open')")
                .bind(p).bind(c).execute(&pool).await.unwrap();
            let fact: Uuid = sqlx::query_scalar("INSERT INTO corporate_offer_facts (program_id, fact_type, value, quote, quote_key, channel, evidence_source,
                                                            pass_b, status, last_outcome, anchors)
                         VALUES ($1, 'presence', '{}'::jsonb, 'Students and teachers save 50% on Live.',
                                 encode(sha256(convert_to('old:' || $1::text, 'UTF8')), 'hex'), 'page', 'audit',
                                 '{}'::jsonb, 'hidden', 'ambiguous', '{}'::jsonb) RETURNING id")
                .bind(p).fetch_one(&pool).await.unwrap();
            sqlx::query("INSERT INTO corporate_evidence_checks (candidate_id, program_id, mode, outcome, applied, details)
                         VALUES ($1, $2, 'live', 'ambiguous', true,
                                 jsonb_build_object('facts', jsonb_build_array(jsonb_build_object(
                                     'fact_id', $3::text, 'outcome', 'ambiguous', 'detail', $4::text))))")
                .bind(c).bind(p).bind(fact).bind(reason).execute(&pool).await.unwrap();
            // Queued for fresh reading, as the evidence job does.
            sqlx::query("INSERT INTO corporate_evidence_targets (candidate_id, reextract_requested_at, reextract_reason)
                         VALUES ($1, now(), 'ambiguous')")
                .bind(c).execute(&pool).await.unwrap();
            (c, p)
        }
    };
    let changed = crate::corporate_auto::resilience::SECTION_CHANGED;
    for (path, mode) in [("/live", Mode::Live), ("/seeded", Mode::Seeded)] {
        let (c, p) = held(path, changed).await;
        pass(&pool, mode, &[c]).await;
        assert_eq!(last(&pool, c).await.0, "awaiting_stability", "{path}");
        backdate(&pool, c).await;
        pass(&pool, mode, &[c]).await;
        assert_eq!(
            last(&pool, c).await.0,
            "published",
            "{path}: {:?}",
            last(&pool, c).await
        );
        let active: bool = sqlx::query_scalar("SELECT is_active FROM programs WHERE id = $1")
            .bind(p)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(active, "{path}: restored by the publish gate");
    }
    // Held, but its request withdrawn (the evidence job clears it once the
    // offer confirms): not a source, no request. Every hold class the
    // evidence job queues is a source
    // (`f4_a_held_fact_of_every_class_reaches_its_exit_and_stays_shown`).
    let (ended, _) = held("/ended", crate::corporate_auto::resilience::PAGE_SAYS_ENDED).await;
    sqlx::query("UPDATE corporate_evidence_targets SET reextract_requested_at = NULL WHERE candidate_id = $1")
        .bind(ended)
        .execute(&pool)
        .await
        .unwrap();
    let before = site.hits("/ended");
    let summary = pass(&pool, Mode::Live, &[ended]).await;
    assert_eq!(summary.sources, 0);
    assert_eq!(site.hits("/ended"), before);
    retire(&pool, &f).await;
}

/// 신한 Hey Young 체크 (2026-11-06): the card page lists CGV and 올리브영
/// cashback, yet states no age or student audience for issuing the card
/// (the 만 18~29세 rule is only in old news). With no audience on the page
/// it is no youth offer, and the gate is not loosened for it.
#[test]
fn a_card_page_that_states_no_youth_audience_stays_refused() {
    let page = offer_page(
        "<section><h2>Hey Young 체크</h2>\
         <p>CGV 영화 예매 5천원 캐시백 건당 2만원 이상 결제 시 5천원 캐시백(월 1회 제공)</p>\
         <p>생활 가맹점 : 올리브영, 다이소 건당 1만원 이상 결제 시 1천원 캐시백</p></section>",
    );
    let refused = decide(
        &page,
        "https://www.shinhancard.com/pconts/html/card/apply/check/1235589_2206.html",
        "신한카드",
    );
    assert!(
        matches!(&refused, Err(reason) if reason.starts_with("reader_a:")),
        "{refused:?}"
    );
}

/// A new offer on a domain whose robots.txt lists the agents that may crawl
/// (named groups allowing `/`, no `*` group, none of them ours) is refused
/// by robots before any page is requested; the same page under an ordinary
/// robots file proceeds.
#[tokio::test]
async fn a_new_offer_on_a_named_agent_allowlist_is_refused_before_any_page_request() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    for (robots, held) in [
        ("User-agent: Yeti\nUser-agent: Googlebot\nAllow: /\n", true),
        // Security review H3: a file whose groups name only other bots is
        // a refusal, whatever its rules say.
        ("User-agent: Googlebot-Image\nDisallow: /images\n", true),
        (robots_allowing(), false),
    ] {
        let site = Site::default();
        site.route("/robots.txt", 200, robots);
        site.route("/education", 200, &offer_page(ABLETON));
        let base = serve(site.clone()).await;
        let f = domain(&pool).await;
        let c = candidate(&pool, &f, &format!("{base}/education"), true).await;
        pass(&pool, Mode::Live, &[c]).await;
        if held {
            assert_eq!(
                last(&pool, c).await,
                ("robots_disallow".into(), "policy".into()),
                "{robots}"
            );
            assert_eq!(site.hits("/education"), 0, "no page request");
        } else {
            assert_eq!(last(&pool, c).await.0, "awaiting_stability", "{robots}");
        }
        retire(&pool, &f).await;
    }
}

/// Review H5/H6 through intake: a door's page is read only while its lane
/// is a live door kind; then under the door brand's scope, through the
/// whole chain (stability, every gate), and the issuer it names is the
/// programme's sponsor, provider and company, with the holder template and
/// the naming line as its condition.
#[tokio::test]
async fn a_door_page_publishes_its_issuer_offer_through_intake() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let gongcha: Uuid = sqlx::query_scalar(
        "SELECT brand_id FROM brand_discovery_targets WHERE target_key = 'gongcha'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // Earlier runs' programmes of this test share 공차's offer identity.
    sqlx::query(
        "DELETE FROM programs WHERE id IN (SELECT program_id FROM corporate_offer_identities
                                           WHERE brand_key = $1::text)",
    )
    .bind(gongcha)
    .execute(&pool)
    .await
    .unwrap();
    let html = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/doors_20261110/pages/gong-cha.co.kr_skt_membership.html"
    ))
    .unwrap();
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/brand/store/skt_membership", 200, &html);
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let c = candidate(
        &pool,
        &f,
        &format!("{base}/brand/store/skt_membership"),
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO corporate_doors (domain_id, entry_url, candidate_id, grantor_brand_id, kind)
         VALUES ($1, $2, $3, $4, 'page')",
    )
    .bind(f.domain_id)
    .bind(format!(
        "https://{}.doorintake.example/brand/store/skt_membership",
        f.domain_id.simple()
    ))
    .bind(c)
    .bind(gongcha)
    .execute(&pool)
    .await
    .unwrap();
    // No live door lane: the door's page is not intake's to read.
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = Some(Vec::new()));
    let summary = pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(summary.sources, 0);
    assert_eq!(site.total(), 0, "no request");
    // The page lane is live.
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = Some(vec!["page".into()]));
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(last(&pool, c).await.0, "awaiting_stability");
    backdate(&pool, c).await;
    pass(&pool, Mode::Live, &[c]).await;
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = None);
    assert_eq!(last(&pool, c).await, ("published".into(), "publish".into()));
    let (program, title, provider, company, condition, scope): (
        Uuid,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT id, title, provider_name, company_name, eligibility_condition,
                    normalized_payload->>'audience_scope'
             FROM programs WHERE source_id = 'intake:' || $1::text",
    )
    .bind(c)
    .fetch_one(&pool)
    .await
    .unwrap();
    // The fact's own sentence (code review H-6).
    assert_eq!(title, "SK텔레콤 · 공차 상시 혜택 10% 할인");
    assert_eq!(
        (provider.as_str(), company.as_str()),
        ("SK텔레콤", "SK텔레콤")
    );
    assert_eq!(
        condition.as_deref(),
        Some("SK텔레콤 휴대폰 이용 고객 · 대상: T 멤버십 할인형 고객")
    );
    assert_eq!(scope.as_deref(), Some("general"));
    // The lead never claims a student audience the page does not name.
    let summary: String = sqlx::query_scalar("SELECT summary FROM programs WHERE id = $1")
        .bind(program)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        summary.starts_with("SK텔레콤 휴대폰 이용 고객은 10% 할인"),
        "{summary}"
    );
    assert!(!summary.contains("학생"), "{summary}");
    let links: Vec<String> = sqlx::query_scalar(
        "SELECT b.name || ':' || pb.relationship_type FROM program_brands pb JOIN brands b ON b.id = pb.brand_id
         WHERE pb.program_id = $1 ORDER BY 1",
    )
    .bind(program)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(links, ["SK텔레콤:sponsor", "공차:redeemable_at"]);
    retire(&pool, &f).await;
}

/// 공차's T 멤버십 page published through intake under its door's brand
/// scope (the setup of the test above): (domain, candidate, programme).
async fn published_door_page(pool: &PgPool) -> (Fixture, Uuid, Uuid) {
    let gongcha: Uuid = sqlx::query_scalar(
        "SELECT brand_id FROM brand_discovery_targets WHERE target_key = 'gongcha'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM programs WHERE id IN (SELECT program_id FROM corporate_offer_identities
                                           WHERE brand_key = $1::text)",
    )
    .bind(gongcha)
    .execute(pool)
    .await
    .unwrap();
    let html = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/doors_20261110/pages/gong-cha.co.kr_skt_membership.html"
    ))
    .unwrap();
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/brand/store/skt_membership", 200, &html);
    let base = serve(site.clone()).await;
    let f = domain(pool).await;
    let c = candidate(
        pool,
        &f,
        &format!("{base}/brand/store/skt_membership"),
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO corporate_doors (domain_id, entry_url, candidate_id, grantor_brand_id, kind)
         VALUES ($1, $2, $3, $4, 'page')",
    )
    .bind(f.domain_id)
    .bind(format!(
        "https://{}.doorintake.example/brand/store/skt_membership",
        f.domain_id.simple()
    ))
    .bind(c)
    .bind(gongcha)
    .execute(pool)
    .await
    .unwrap();
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = Some(vec!["page".into()]));
    pass(pool, Mode::Live, &[c]).await;
    backdate(pool, c).await;
    pass(pool, Mode::Live, &[c]).await;
    assert_eq!(last(pool, c).await, ("published".into(), "publish".into()));
    let program: Uuid =
        sqlx::query_scalar("SELECT id FROM programs WHERE source_id = 'intake:' || $1::text")
            .bind(c)
            .fetch_one(pool)
            .await
            .unwrap();
    (f, c, program)
}

/// Code review H-8 / verify F5: a held brand-scope programme whose quote is
/// gone is re-adopted through the publish gate. The stored condition's
/// holder template is server text, checked against the issuer's key; only
/// the issuer line is checked against the page.
#[tokio::test]
async fn brand_scope_offer_returns_after_hold() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let (f, c, program) = published_door_page(&pool).await;
    // Shown first (the evidence job confirmed it), then held.
    for sql in [
        "UPDATE program_source_holds SET resolved_at = now(), resolution = 'confirmed_unchanged'
         WHERE program_id = $1 AND resolved_at IS NULL",
        "UPDATE programs SET is_active = false, program_status = 'review_required', source_health = 'review_due'
         WHERE id = $1",
        "UPDATE program_source_monitors SET verification_state = 'change_detected' WHERE program_id = $1",
        "INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
         SELECT $1, candidate_id, 'evidence_failed', true, 'always_open' FROM program_source_monitors WHERE program_id = $1",
        "UPDATE corporate_offer_facts SET status = 'hidden', last_outcome = 'quote_missing',
                quote = '사라진 문장 10% 할인' WHERE program_id = $1",
        // Queued for fresh reading, as the evidence job does.
        "INSERT INTO corporate_evidence_targets (candidate_id, reextract_requested_at, reextract_reason)
         SELECT candidate_id, now(), 'quote_missing' FROM program_source_monitors WHERE program_id = $1
         ON CONFLICT (candidate_id) DO UPDATE SET reextract_requested_at = now()",
    ] {
        sqlx::query(sql).bind(program).execute(&pool).await.unwrap();
    }
    sqlx::query("DELETE FROM corporate_intake_offers WHERE candidate_id = $1")
        .bind(c)
        .execute(&pool)
        .await
        .unwrap();
    pass(&pool, Mode::Live, &[c]).await;
    assert_eq!(last(&pool, c).await.0, "awaiting_stability");
    backdate(&pool, c).await;
    pass(&pool, Mode::Live, &[c]).await;
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = None);
    assert_eq!(
        last(&pool, c).await.0,
        "published",
        "{:?}",
        last(&pool, c).await
    );
    let (active, condition): (bool, Option<String>) =
        sqlx::query_as("SELECT is_active, eligibility_condition FROM programs WHERE id = $1")
            .bind(program)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(active, "restored by the publish gate");
    assert_eq!(
        condition.as_deref(),
        Some("SK텔레콤 휴대폰 이용 고객 · 대상: T 멤버십 할인형 고객")
    );
    retire(&pool, &f).await;
}

/// Code review H-8: the weekly audit reads a door page's programme under
/// the door's brand scope and agrees with it.
#[tokio::test]
async fn weekly_audit_reads_brand_scope_offer() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let (f, _c, program) = published_door_page(&pool).await;
    super::TEST_DOOR_LANES.with(|lanes| *lanes.borrow_mut() = None);
    let http = LoopbackHttp::new(UA);
    let summary = crate::corporate_auto::weekly_audit::run_with(
        &pool,
        crate::corporate_auto::VerifyMode::Live,
        true,
        &http,
        Some(vec![program]),
        crate::corporate_auto::weekly_audit::Budget::default(),
        Some(std::time::Duration::from_millis(5)),
        loopback,
        publish::PAGE_ENDING_SCAN,
    )
    .await
    .unwrap();
    assert_eq!(
        (summary.agrees, summary.disagrees, summary.unreadable),
        (1, 0, 0),
        "{summary:?}"
    );
    retire(&pool, &f).await;
}

// ── F4: every hold class has one exit, through one queue ─────────────────

/// A held corporate source as the evidence job leaves it: one `evidence_auto`
/// programme hidden by an `evidence_failed` hold, its fact last read with
/// `outcome` (and `reason`, for `ambiguous`), and the source queued for
/// fresh reading (`corporate_evidence_targets.reextract_requested_at`).
async fn held_source(
    pool: &PgPool,
    f: &Fixture,
    url: &str,
    outcome: &str,
    reason: Option<&str>,
) -> (Uuid, Uuid) {
    let c = Uuid::new_v4();
    let p = Uuid::new_v4();
    sqlx::query("INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
                 VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'approved', 'detail')")
        .bind(c).bind(f.domain_id).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, official_url,
                                       program_status, provider_kind, is_active, source_health, regions)
                 VALUES ($1, 'corporate_benefit', 'corporate_official', $2, 'Ableton 학생 할인', '학생 할인', 'Ableton', $3,
                         'review_required', 'corporate', false, 'review_due', ARRAY[]::text[])")
        .bind(p).bind(format!("held:{p}")).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at, review_due_at, verification_mode)
                 VALUES ($1, $2, 'change_detected', now() - interval '20 days', now() + interval '10 days', 'evidence_auto')")
        .bind(p).bind(c).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                 VALUES ($1, $2, 'evidence_failed', true, 'always_open')")
        .bind(p).bind(c).execute(pool).await.unwrap();
    if outcome != "no_evidence" {
        let fact: Uuid = sqlx::query_scalar("INSERT INTO corporate_offer_facts (program_id, fact_type, value, quote, quote_key, channel, evidence_source,
                                                        pass_b, status, last_outcome, anchors, window_signature)
                     VALUES ($1, 'presence', '{}'::jsonb, 'Students and teachers save 50% on Live.',
                             encode(sha256(convert_to('old:' || $1::text, 'UTF8')), 'hex'), 'page', 'audit',
                             '{}'::jsonb, 'hidden', $2, '{}'::jsonb, '5만원,5만원,5천원') RETURNING id")
            .bind(p).bind(outcome).fetch_one(pool).await.unwrap();
        sqlx::query("INSERT INTO corporate_evidence_checks (candidate_id, program_id, mode, outcome, applied, details)
                     VALUES ($1, $2, 'live', $3, true,
                             jsonb_build_object('facts', jsonb_build_array(jsonb_build_object(
                                 'fact_id', $4::text, 'outcome', $3, 'detail', $5::text))))")
            .bind(c).bind(p).bind(outcome).bind(fact).bind(reason).execute(pool).await.unwrap();
    }
    sqlx::query("INSERT INTO corporate_evidence_targets (candidate_id, last_outcome, reextract_requested_at, reextract_reason, next_check_at)
                 VALUES ($1, 'evidence_failed', now() - interval '1 day', $2, now() + interval '1 day')")
        .bind(c).bind(outcome).execute(pool).await.unwrap();
    (c, p)
}

async fn active(pool: &PgPool, program: Uuid) -> bool {
    sqlx::query_scalar("SELECT is_active FROM programs WHERE id = $1")
        .bind(program)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// RC2 (production 2026-11-11: 19 held rows, 6 with no exit at all): a held
/// fact of every class that rereading can settle (quote gone, value window
/// or field changed, section changed, a date or an ending read, a baseline
/// of another reader, an adoption that ran out, no facts) reaches the one
/// exit, the held-source lane of intake, fed by the evidence job's queue;
/// it is republished only through the whole chain (policy, readers A and
/// B, scope, the ending scan, 24 h stability), and the next evidence pass
/// on the same page confirms it (no flap).
#[tokio::test]
async fn f4_a_held_fact_of_every_class_reaches_its_exit_and_stays_shown() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    use crate::corporate_auto::resilience as r;
    let classes: &[(&str, Option<&str>)] = &[
        ("quote_missing", None),
        ("window_changed", None),
        ("field_mismatch", None),
        ("ambiguous", Some(r::SECTION_CHANGED)),
        ("ambiguous", Some(r::SECTION_DATE_OTHER)),
        ("ambiguous", Some(r::PAGE_SAYS_ENDED)),
        ("ambiguous", Some(r::BASELINE_UNREADABLE)),
        ("adoption_expired", None),
        ("no_evidence", None),
    ];
    for (n, (outcome, reason)) in classes.iter().enumerate() {
        let path = format!("/held{n}");
        site.route(&path, 200, &offer_page(ABLETON));
        let (c, p) = held_source(&pool, &f, &format!("{base}{path}"), outcome, *reason).await;
        let what = format!("{outcome} {reason:?}");
        pass(&pool, Mode::Seeded, &[c]).await;
        assert_eq!(last(&pool, c).await.0, "awaiting_stability", "{what}");
        assert!(!active(&pool, p).await, "{what}: hidden until stable");
        backdate(&pool, c).await;
        pass(&pool, Mode::Seeded, &[c]).await;
        assert_eq!(
            last(&pool, c).await.0,
            "published",
            "{what}: {:?}",
            last(&pool, c).await
        );
        assert!(
            active(&pool, p).await,
            "{what}: restored by the publish gate"
        );
        // The next evidence pass on the same page confirms: no flap.
        sqlx::query(
            "UPDATE corporate_evidence_targets SET next_check_at = now() WHERE candidate_id = $1",
        )
        .bind(c)
        .execute(&pool)
        .await
        .unwrap();
        crate::corporate_auto::evidence::run_with(
            &pool,
            crate::corporate_auto::VerifyMode::Live,
            &LoopbackHttp::new(UA),
            Some(vec![c]),
            Some(std::time::Duration::from_millis(5)),
        )
        .await
        .unwrap();
        let outcome: String = sqlx::query_scalar(
            "SELECT outcome FROM corporate_evidence_checks WHERE candidate_id = $1 ORDER BY checked_at DESC, id DESC LIMIT 1",
        )
        .bind(c).fetch_one(&pool).await.unwrap();
        assert_eq!(outcome, "confirmed", "{what}: confirmed after republish");
        assert!(active(&pool, p).await, "{what}: still shown");
    }
    retire(&pool, &f).await;
}

/// Must refuse: a held source whose page now says the offer ended is read
/// and refused by the chain; it stays hidden. A source the evidence job
/// did not queue (its request withdrawn) is not read at all.
#[tokio::test]
async fn f4_a_held_source_whose_page_says_ended_stays_hidden() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route(
        "/ended",
        200,
        &offer_page(
            "<section><h2>Students and teachers</h2>\
             <p>Students and teachers get a 50% discount on a new Live license.</p>\
             <p>This offer has ended.</p></section>",
        ),
    );
    site.route("/withdrawn", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let (c, p) = held_source(
        &pool,
        &f,
        &format!("{base}/ended"),
        "ambiguous",
        Some(crate::corporate_auto::resilience::PAGE_SAYS_ENDED),
    )
    .await;
    for _ in 0..2 {
        pass(&pool, Mode::Live, &[c]).await;
        backdate(&pool, c).await;
    }
    assert_ne!(last(&pool, c).await.0, "published");
    assert!(site.hits("/ended") > 0, "the page was read");
    assert!(!active(&pool, p).await, "an ended offer stays hidden");

    let (w, _) = held_source(
        &pool,
        &f,
        &format!("{base}/withdrawn"),
        "quote_missing",
        None,
    )
    .await;
    sqlx::query("UPDATE corporate_evidence_targets SET reextract_requested_at = NULL WHERE candidate_id = $1")
        .bind(w).execute(&pool).await.unwrap();
    let summary = pass(&pool, Mode::Live, &[w]).await;
    assert_eq!(summary.sources, 0);
    assert_eq!(site.hits("/withdrawn"), 0);
    retire(&pool, &f).await;
}

/// F5 (RC5): a programme on a person's timer whose page is direct HTML and
/// open to our agent moves to automatic evidence in `seeded` mode too: its
/// URL is the one a person reviewed when it was seeded, the same ground
/// on which a held source republishes. It goes through the whole chain and
/// the publish gate; nothing typed by a person is kept.
#[tokio::test]
async fn f5_seeded_moves_a_readable_human_source_to_automatic_evidence() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::default();
    site.route("/robots.txt", 200, robots_allowing());
    site.route("/education", 200, &offer_page(ABLETON));
    let base = serve(site.clone()).await;
    let f = domain(&pool).await;
    let c = Uuid::new_v4();
    let p = Uuid::new_v4();
    let url = format!("{base}/education");
    sqlx::query("INSERT INTO benefit_source_candidates (id, domain_id, url, canonical_url, discovered_via, status, candidate_kind)
                 VALUES ($1, $2, $3, $3 || '#' || $1::text, 'manual', 'verification_due', 'detail')")
        .bind(c).bind(f.domain_id).bind(&url).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, official_url,
                                       program_status, provider_kind, is_active, source_health, regions)
                 VALUES ($1, 'corporate_benefit', 'corporate_official', $2, '사람이 쓴 제목', '사람이 쓴 요약', 'Ableton', $3,
                         'review_required', 'corporate', false, 'review_due', ARRAY[]::text[])")
        .bind(p).bind(format!("human:{p}")).bind(&url).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at, review_due_at, verification_mode)
                 VALUES ($1, $2, 'review_due', now() - interval '60 days', now() - interval '30 days', 'human')")
        .bind(p).bind(c).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                 VALUES ($1, $2, 'verification_overdue', true, 'always_open')")
        .bind(p).bind(c).execute(&pool).await.unwrap();
    pass(&pool, Mode::Seeded, &[c]).await;
    assert_eq!(last(&pool, c).await.0, "awaiting_stability");
    backdate(&pool, c).await;
    pass(&pool, Mode::Seeded, &[c]).await;
    assert_eq!(
        last(&pool, c).await,
        ("published".to_string(), "publish".to_string())
    );
    let (mode, active, title): (String, bool, String) = sqlx::query_as(
        "SELECT m.verification_mode, p.is_active, p.title FROM program_source_monitors m JOIN programs p ON p.id = m.program_id WHERE p.id = $1",
    )
    .bind(p).fetch_one(&pool).await.unwrap();
    assert_eq!(mode, "evidence_auto");
    assert!(active);
    assert_ne!(title, "사람이 쓴 제목", "composed from the page's facts");
    retire(&pool, &f).await;
}

// ── F7 (contained): the reader-capability backlog as a labelled set ─────

/// RC6: the offers a person confirmed on 2026-10-16 that the fresh chain
/// refuses, on their saved pages. Each is a page shape the reader does not
/// handle yet; the accuracy gates are not loosened for them. The verdicts
/// are the backlog's labelled set: a reader change that publishes one must
/// do it through the same gates and update this list deliberately; one that
/// changes a refusal into another refusal shows here too.
#[test]
fn the_reader_capability_backlog_is_measured_on_saved_pages() {
    let pages: &[(&str, &str, &str)] = &[
        (
            "4501ce12.html",
            "https://www.notion.com/help/notion-for-education",
            "Notion",
        ),
        (
            "4067fb7a.html",
            "https://www.microsoft.com/ko-kr/education/products/office",
            "Microsoft",
        ),
        (
            "d77ca111.html",
            "https://education.github.com/pack",
            "GitHub",
        ),
        (
            "0d2d2070.html",
            "https://www.ikea.com/kr/ko/ikea-family/benefits/",
            "IKEA",
        ),
        (
            "18987f71.html",
            "https://1password.com/developers/students",
            "1Password",
        ),
        (
            "1249aeda.html",
            "https://www.sketch.com/education/",
            "Sketch",
        ),
        (
            "5cc5dd4a.html",
            "https://linear.app/docs/billing-and-plans",
            "Linear",
        ),
        (
            "eeef56dc.html",
            "https://aws.amazon.com/ko/education/awseducate/",
            "AWS",
        ),
        (
            "0247bfc3.html",
            "https://m.tworld.co.kr/product/callplan?prod_id=NA00009785",
            "SKT",
        ),
        (
            "ae07f4c8.html",
            "https://www.adobe.com/kr/education/students/creativecloud.html",
            "Adobe",
        ),
        (
            "7d66fe55.html",
            "https://www.apple.com/kr-edu/store",
            "Apple",
        ),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_pages");
    let mut verdicts = Vec::new();
    for (file, url, brand) in pages {
        let html = std::fs::read_to_string(dir.join(file)).unwrap();
        let verdict = match decide(&html, url, brand) {
            Ok(checked) => format!("passes: {}", checked.title),
            Err(reason) => reason.split(':').take(2).collect::<Vec<_>>().join(":"),
        };
        println!("BACKLOG {brand} {verdict}");
        verdicts.push((*brand, verdict));
    }
    let expected = [
        ("Notion", "reader_a:multi_offer_page"),
        ("Microsoft", "reader_a:no_student_youth_offer"),
        ("GitHub", "reader_a:multi_offer_page"),
        ("IKEA", "reader_a:product_listing"),
        ("1Password", "reader_b:quote_readback"),
        ("Sketch", "reader_a:conflicting_terms"),
        ("Linear", "ending_scan:offer_ended_scan"),
        ("AWS", "reader_a:no_student_youth_offer"),
        ("SKT", "reader_b:section"),
        ("Adobe", "reader_a:template_shell"),
        ("Apple", "reader_a:no_student_youth_offer"),
    ];
    let expected: Vec<(&str, String)> = expected.iter().map(|(b, v)| (*b, v.to_string())).collect();
    assert_eq!(
        verdicts, expected,
        "the backlog moved: a page that now passes must be checked right before it leaves the list"
    );
}
