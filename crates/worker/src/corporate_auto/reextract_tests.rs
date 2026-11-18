use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";

static DB_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn filler() -> String {
    "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12)
}

/// A redesigned page that still states one student offer.
fn offer_page(percent: u32, terms_link: bool) -> String {
    let terms = if terms_link {
        "<footer><a href=\"/terms\">이용약관</a></footer>"
    } else {
        ""
    };
    format!(
        "<html><head><title>카페 학생 혜택</title></head><body><main class=\"v2-layout\">\
         <section><h2>학생 혜택</h2><p>대학생은 학생증 제시 시 전 메뉴 {percent}% 할인 혜택을 받을 수 있습니다.</p></section>\
         <p>{}</p></main>{terms}</body></html>",
        filler()
    )
}

// ── pure ─────────────────────────────────────────────────────────────────

#[test]
fn the_fixture_page_is_an_extractable_offer() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
    let offer = bootstrap::extract_offer(&offer_page(20, false), today).expect("offer");
    assert!(offer
        .facts
        .iter()
        .any(|f| f.kind == "discount_percent" && f.value == json!(20)));
}

#[test]
fn retry_delay_doubles_for_transient_failures_and_is_fixed_for_policy() {
    assert_eq!(retry_delay(OutcomeClass::Transient, 1), Duration::hours(6));
    assert_eq!(retry_delay(OutcomeClass::Transient, 2), Duration::hours(12));
    assert_eq!(retry_delay(OutcomeClass::Transient, 3), Duration::hours(24));
    assert_eq!(retry_delay(OutcomeClass::Transient, 9), Duration::days(7));
    assert_eq!(retry_delay(OutcomeClass::Policy, 1), Duration::days(7));
    assert_eq!(classify("proposal"), OutcomeClass::Proposal);
    assert_eq!(classify("offer_ended"), OutcomeClass::NoOffer);
    assert_eq!(classify("page_unreachable"), OutcomeClass::Transient);
    for policy in [
        "robots_disallow",
        "tos_specific_collection",
        "opted_out",
        "bot_wall",
        "campus_out_of_scope",
        "policy_blocked",
        "not_evidence_auto",
        "lane_js_shell",
        "js_shell",
    ] {
        assert_eq!(classify(policy), OutcomeClass::Policy, "{policy}");
    }
}

// ── database + mock official site ────────────────────────────────────────

#[derive(Clone, Default)]
struct Site {
    robots: Arc<Mutex<(u16, String)>>,
    page: Arc<Mutex<(u16, String)>>,
    terms: Arc<Mutex<(u16, String)>>,
    robots_hits: Arc<AtomicUsize>,
    page_hits: Arc<AtomicUsize>,
    /// When set, `/benefit` answers 302 to this same-host path, and that
    /// path (or the part of it the transport keeps) serves the page.
    redirect: Arc<Mutex<Option<String>>>,
    user_agents: Arc<Mutex<Vec<String>>>,
}

impl Site {
    fn new(page: String) -> Self {
        let site = Self::default();
        *site.robots.lock().unwrap() = (200, "User-agent: *\nAllow: /\n".into());
        *site.page.lock().unwrap() = (200, page);
        *site.terms.lock().unwrap() = (404, "missing".into());
        site
    }

    fn requests(&self) -> usize {
        self.robots_hits.load(Ordering::SeqCst) + self.page_hits.load(Ordering::SeqCst)
    }
}

async fn serve(site: Site) -> String {
    use axum::{http::HeaderMap, response::IntoResponse, routing::get, Router};
    let reply = |status: u16, kind: &'static str, body: String| {
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            [("content-type", kind)],
            body,
        )
            .into_response()
    };
    let (robots_site, page_site, terms_site) = (site.clone(), site.clone(), site.clone());
    let app = Router::new()
        .route(
            "/robots.txt",
            get(move |headers: HeaderMap| {
                let site = robots_site.clone();
                async move {
                    site.robots_hits.fetch_add(1, Ordering::SeqCst);
                    site.user_agents
                        .lock()
                        .unwrap()
                        .push(headers["user-agent"].to_str().unwrap().to_string());
                    let (status, body) = site.robots.lock().unwrap().clone();
                    reply(status, "text/plain", body)
                }
            }),
        )
        .route(
            "/benefit",
            get(move |headers: HeaderMap| {
                let site = page_site.clone();
                async move {
                    site.page_hits.fetch_add(1, Ordering::SeqCst);
                    site.user_agents
                        .lock()
                        .unwrap()
                        .push(headers["user-agent"].to_str().unwrap().to_string());
                    if let Some(location) = site.redirect.lock().unwrap().clone() {
                        let mut response = reply(302, "text/html; charset=utf-8", String::new());
                        response
                            .headers_mut()
                            .insert("location", location.parse().unwrap());
                        return response;
                    }
                    let (status, body) = site.page.lock().unwrap().clone();
                    reply(status, "text/html; charset=utf-8", body)
                }
            }),
        )
        .route(
            "/terms",
            get(move || {
                let site = terms_site.clone();
                async move {
                    let (status, body) = site.terms.lock().unwrap().clone();
                    reply(status, "text/html; charset=utf-8", body)
                }
            }),
        )
        .fallback({
            let site = site.clone();
            move |request: axum::extract::Request| {
                let site = site.clone();
                async move {
                    let path = request.uri().path().to_string();
                    let target = site.redirect.lock().unwrap().clone();
                    // The transport keeps at most 2,000 characters of a
                    // Location header, so a long target arrives cut.
                    if target.is_some_and(|t| t.starts_with(&path)) {
                        site.page_hits.fetch_add(1, Ordering::SeqCst);
                        let (status, body) = site.page.lock().unwrap().clone();
                        return reply(status, "text/html; charset=utf-8", body);
                    }
                    reply(404, "text/html; charset=utf-8", "missing".into())
                }
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/benefit")
}

/// The loopback site speaks plain http on a random port. The real
/// [`bootstrap::precheck`] still runs, on the https URL the row would have
/// on its own domain; the loopback URL is what is fetched.
fn loopback_check(row: &CandidateRow) -> std::result::Result<Url, &'static str> {
    let fetched = Url::parse(&row.url).map_err(|_| "invalid_url")?;
    let mut as_published = row.clone();
    let query = fetched.query().map(|q| format!("?{q}")).unwrap_or_default();
    as_published.url = format!("https://{}{}{query}", row.domain, fetched.path());
    bootstrap::precheck(&as_published)?;
    Ok(fetched)
}

struct Fixture {
    candidate_id: Uuid,
    program_id: Uuid,
    domain_id: Uuid,
    brand_id: Uuid,
}

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

/// A hidden corporate offer whose evidence failed after a redesign: the
/// evidence job left `reextract_requested_at` on its source.
async fn fixture(pool: &PgPool, url: &str) -> Fixture {
    let domain_id = Uuid::new_v4();
    let candidate_id = Uuid::new_v4();
    let program_id = Uuid::new_v4();
    let brand_id = Uuid::new_v4();
    let domain = format!("{}.reextract.example", domain_id.simple());
    sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
                 VALUES($1,'reextract fixture',$2,'brand_official','verified','fixture',now(),'pending','manual_review')")
        .bind(domain_id).bind(&domain).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at)
                 VALUES($1,$2,$3,$3 || '#' || $1::text,'manual','verification_due','detail',now())")
        .bind(candidate_id).bind(domain_id).bind(url).execute(pool).await.unwrap();
    let brand_name = format!("픽스처카페-{}", &brand_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO brands(id,name,normalized_name,source_type,source_id) VALUES($1,$2,$2,'fixture',$1::text)")
        .bind(brand_id)
        .bind(&brand_name)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO benefit_candidate_brands(candidate_id,brand_id) VALUES($1,$2)")
        .bind(candidate_id)
        .bind(brand_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                 VALUES($1,'corporate_benefit','corporate_official',$2,'픽스처 카페 학생 할인','대학생 10% 할인','픽스처카페',$3,'review_required','corporate','discount',false,'review_due',ARRAY[]::text[])")
        .bind(program_id).bind(format!("fixture:{program_id}")).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode)
                 VALUES($1,$2,'review_due',now()-interval '60 days',now()-interval '30 days','evidence_auto')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'evidence_failed',true,'always_open')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    let quote = "학생증 제시 시 10% 할인";
    sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,status)
                 VALUES($1,'presence','{}'::jsonb,$2,$3,'fixture','hidden')")
        .bind(program_id).bind(quote).bind(format!("{:064x}", program_id.as_u128()))
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO corporate_evidence_targets(candidate_id,last_outcome,reextract_requested_at,reextract_reason)
                 VALUES($1,'evidence_failed',now()-interval '1 hour','quote_missing')")
        .bind(candidate_id).execute(pool).await.unwrap();
    Fixture {
        candidate_id,
        program_id,
        domain_id,
        brand_id,
    }
}

/// Everything public or user-facing about the fixture's offer.
async fn public_snapshot(pool: &PgPool, f: &Fixture) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'program', (SELECT to_jsonb(p) - 'updated_at' FROM programs p WHERE p.id = $1),
             'monitors', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.candidate_id) FROM program_source_monitors m WHERE m.program_id = $1),
             'holds', (SELECT jsonb_agg(to_jsonb(h) ORDER BY to_jsonb(h)::text) FROM program_source_holds h WHERE h.program_id = $1),
             'facts', (SELECT jsonb_agg(to_jsonb(x) ORDER BY x.id) FROM corporate_offer_facts x WHERE x.program_id = $1),
             'target', (SELECT to_jsonb(t) FROM corporate_evidence_targets t WHERE t.candidate_id = $2),
             'candidate', (SELECT to_jsonb(c) FROM benefit_source_candidates c WHERE c.id = $2),
             'programs_total', (SELECT count(*) FROM programs),
             'active_programs', (SELECT count(*) FROM programs WHERE is_active))",
    )
    .bind(f.program_id)
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn pass(pool: &PgPool, f: &[&Fixture], mode: VerifyMode, cap: i64) -> ReextractSummary {
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        mode,
        mode == VerifyMode::Live,
        true,
        &http,
        Some(f.iter().map(|f| f.candidate_id).collect()),
        cap,
        Some(std::time::Duration::from_millis(5)),
        loopback_check,
        None,
    )
    .await
    .unwrap()
}

#[derive(Debug, FromRow)]
struct StateRow {
    handled_request_at: Option<DateTime<Utc>>,
    consecutive_failures: i32,
    attempts: i32,
    next_attempt_at: DateTime<Utc>,
    last_outcome: Option<String>,
}

async fn state(pool: &PgPool, f: &Fixture) -> Option<StateRow> {
    sqlx::query_as(
        "SELECT handled_request_at, consecutive_failures, attempts, next_attempt_at, last_outcome
         FROM corporate_reextract_state WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn requested_at(pool: &PgPool, f: &Fixture) -> Option<DateTime<Utc>> {
    sqlx::query_scalar(
        "SELECT reextract_requested_at FROM corporate_evidence_targets WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[derive(Debug, FromRow)]
struct ProposalRow {
    status: String,
    mode: String,
    sightings: i32,
    offer: Value,
    gates: Value,
    program_ids: Vec<Uuid>,
    request_reason: Option<String>,
}

async fn proposals(pool: &PgPool, f: &Fixture) -> Vec<ProposalRow> {
    sqlx::query_as(
        "SELECT status, mode, sightings, offer, gates, program_ids, request_reason
         FROM corporate_reextract_proposals WHERE candidate_id = $1 ORDER BY first_seen_at, id",
    )
    .bind(f.candidate_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn event(pool: &PgPool, f: &Fixture) -> Option<(String, i32)> {
    sqlx::query_as("SELECT status, occurrence_count FROM operational_events WHERE fingerprint = $1")
        .bind(format!("corporate_reextract:{}", f.candidate_id))
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn due_now(pool: &PgPool, f: &Fixture) {
    sqlx::query(
        "UPDATE corporate_reextract_state SET next_attempt_at = now() WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn cleanup(pool: &PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
        .bind(format!("corporate_reextract:{}", f.candidate_id))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(f.program_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id = $1")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM brands WHERE id = $1")
        .bind(f.brand_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(f.domain_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_flagged_source_is_re_extracted_into_a_proposal_and_nothing_public_changes() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20, false));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let before = public_snapshot(&pool, &f).await;
    let requested = requested_at(&pool, &f).await;

    // Shadow: flag set → fresh extraction → one proposal.
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(summary.sources, 1);
    assert_eq!(summary.proposals, 1, "{:?}", summary.outcomes);
    let rows = proposals(&pool, &f).await;
    assert_eq!(rows.len(), 1);
    let p = &rows[0];
    assert_eq!(
        (p.status.as_str(), p.mode.as_str(), p.sightings),
        ("proposed", "shadow", 1)
    );
    assert_eq!(p.program_ids, vec![f.program_id]);
    assert_eq!(p.request_reason.as_deref(), Some("quote_missing"));
    let facts = p.offer["offer"]["facts"].as_array().unwrap();
    assert!(facts
        .iter()
        .any(|x| x["kind"] == "discount_percent" && x["value"] == 20));
    for fact in facts {
        let quote = fact["quote"].as_str().unwrap();
        assert!(!quote.is_empty() && quote.chars().count() <= MAX_QUOTE_CHARS);
    }
    assert_eq!(p.offer["previous_values"][0]["values"], "10%");
    assert_eq!(p.gates["lane"], "direct");
    assert_eq!(p.gates["lane_reason"], "direct_html");
    assert_eq!(p.gates["robots"], "ok");
    assert_eq!(p.gates["quotes_verbatim"], true);
    assert!(site.user_agents.lock().unwrap().iter().all(|ua| ua == UA));

    // The request is consumed once: handled, not cleared (the evidence job
    // owns the flag), and the offer stays exactly as it was.
    let s = state(&pool, &f).await.unwrap();
    assert_eq!(s.handled_request_at, requested);
    assert_eq!(
        (
            s.consecutive_failures,
            s.attempts,
            s.last_outcome.as_deref()
        ),
        (0, 1, Some("proposal"))
    );
    assert_eq!(requested_at(&pool, &f).await, requested);
    assert_eq!(public_snapshot(&pool, &f).await, before);

    // Idempotent: running again fetches nothing and writes nothing.
    let hits = site.requests();
    let again = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(again.sources, 0);
    assert_eq!(site.requests(), hits);
    assert_eq!(proposals(&pool, &f).await[0].sightings, 1);

    // A new request (the flag cleared and set again) is consumed once more.
    // Live mode proposes too, and still changes nothing public.
    sqlx::query("UPDATE corporate_evidence_targets SET reextract_requested_at = now() WHERE candidate_id = $1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    let live = pass(&pool, &[&f], VerifyMode::Live, 10).await;
    assert_eq!(live.proposals, 1);
    let rows = proposals(&pool, &f).await;
    assert_eq!(rows.len(), 1, "same content, same proposal");
    assert_eq!((rows[0].sightings, rows[0].mode.as_str()), (2, "live"));

    // The page changes again: the old proposal is superseded, not deleted.
    *site.page.lock().unwrap() = (200, offer_page(25, false));
    due_now(&pool, &f).await;
    pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    let rows = proposals(&pool, &f).await;
    assert_eq!(
        rows.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
        vec!["superseded", "proposed"]
    );
    let after = public_snapshot(&pool, &f).await;
    // Only the flag timestamp moved (written by this test).
    let mut expected = before.clone();
    expected["target"]["reextract_requested_at"] =
        after["target"]["reextract_requested_at"].clone();
    assert_eq!(after, expected);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn repeated_failures_back_off_and_raise_one_event_until_success() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(String::new());
    *site.page.lock().unwrap() = (503, "unavailable".into());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let before = public_snapshot(&pool, &f).await;

    let first = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(first.proposals, 0);
    let s = state(&pool, &f).await.unwrap();
    assert_eq!(s.consecutive_failures, 1);
    assert_eq!(
        s.handled_request_at, None,
        "a failure never advances the request"
    );
    assert_eq!(s.last_outcome.as_deref(), Some("page_unreachable"));
    let wait = s.next_attempt_at - Utc::now();
    assert!(
        wait > Duration::hours(5) && wait <= Duration::hours(6),
        "{wait}"
    );
    assert!(event(&pool, &f).await.is_none());

    // Inside the backoff nothing is fetched.
    let hits = site.requests();
    assert_eq!(pass(&pool, &[&f], VerifyMode::Shadow, 10).await.sources, 0);
    assert_eq!(site.requests(), hits);

    due_now(&pool, &f).await;
    pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    let s = state(&pool, &f).await.unwrap();
    assert_eq!(s.consecutive_failures, 2);
    let wait = s.next_attempt_at - Utc::now();
    assert!(
        wait > Duration::hours(11) && wait <= Duration::hours(12),
        "{wait}"
    );
    assert!(event(&pool, &f).await.is_none());

    due_now(&pool, &f).await;
    let third = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(third.events_raised, 1);
    assert_eq!(state(&pool, &f).await.unwrap().consecutive_failures, 3);
    assert_eq!(event(&pool, &f).await, Some(("open".into(), 1)));
    // A fourth failure updates the same event, it does not add one.
    due_now(&pool, &f).await;
    pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(event(&pool, &f).await, Some(("open".into(), 2)));
    assert!(proposals(&pool, &f).await.is_empty());
    assert_eq!(public_snapshot(&pool, &f).await, before);

    // The site recovers: proposal, counter reset, event resolved.
    *site.page.lock().unwrap() = (200, offer_page(20, false));
    due_now(&pool, &f).await;
    let ok = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(ok.proposals, 1);
    let s = state(&pool, &f).await.unwrap();
    assert_eq!(s.consecutive_failures, 0);
    assert!(s.handled_request_at.is_some());
    assert_eq!(event(&pool, &f).await.unwrap().0, "resolved");
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn policy_gates_apply_as_for_a_new_candidate() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;

    // robots disallows the page for our agent: robots read, page never.
    let site = Site::new(offer_page(20, false));
    *site.robots.lock().unwrap() = (200, "User-agent: *\nDisallow: /benefit\n".into());
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let before = public_snapshot(&pool, &f).await;
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(summary.outcomes.get("robots_disallow"), Some(&1));
    assert_eq!(site.page_hits.load(Ordering::SeqCst), 0);
    let s = state(&pool, &f).await.unwrap();
    assert!(s.handled_request_at.is_none());
    assert!(s.next_attempt_at - Utc::now() > Duration::days(6));
    assert!(proposals(&pool, &f).await.is_empty());
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;

    // Terms that name collection for third parties stop the domain.
    let site = Site::new(offer_page(20, true));
    *site.terms.lock().unwrap() = (
        200,
        format!(
            "<html><body><p>크롤링 등 자동화된 방법으로 사이트의 가격 및 이벤트 정보를 수집하여 제3자에게 제공하는 행위를 금지합니다.</p><p>{}</p></body></html>",
            filler()
        ),
    );
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(
        summary.outcomes.get("tos_specific_collection"),
        Some(&1),
        "{:?}",
        summary.outcomes
    );
    assert!(proposals(&pool, &f).await.is_empty());
    cleanup(&pool, &f).await;

    // A generic automated-access clause proceeds only while G1 is on.
    let site = Site::new(offer_page(20, true));
    *site.terms.lock().unwrap() = (
        200,
        format!(
            "<html><body><p>로봇, 스파이더 등 자동화된 수단으로 서비스에 접근하는 행위를 금지합니다.</p><p>{}</p></body></html>",
            filler()
        ),
    );
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let http = LoopbackHttp::new(UA);
    let g1_off = run_with(
        &pool,
        VerifyMode::Shadow,
        false,
        false,
        &http,
        Some(vec![f.candidate_id]),
        10,
        Some(std::time::Duration::from_millis(5)),
        loopback_check,
        None,
    )
    .await
    .unwrap();
    assert_eq!(g1_off.outcomes.get("tos_generic_gray_off"), Some(&1));
    assert!(proposals(&pool, &f).await.is_empty());
    due_now(&pool, &f).await;
    let g1_on = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(g1_on.proposals, 1, "{:?}", g1_on.outcomes);
    assert_eq!(
        proposals(&pool, &f).await[0].gates["tos"],
        "generic_automated"
    );
    cleanup(&pool, &f).await;

    // A publisher opt-out: no request at all, robots included.
    let site = Site::new(offer_page(20, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let optout: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts(domain,scope,channel,effective_at) VALUES('127.0.0.1','collection','web_form',now()) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    sqlx::query("DELETE FROM benefit_source_optouts WHERE id = $1")
        .bind(optout)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(summary.outcomes.get("opted_out"), Some(&1));
    assert_eq!(site.requests(), 0);
    cleanup(&pool, &f).await;

    // No brand (a campus source), a domain that is not eligible, a monitor
    // a person took back: refused before any request.
    let site = Site::new(offer_page(20, false));
    let campus = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("DELETE FROM benefit_candidate_brands WHERE candidate_id = $1")
        .bind(campus.candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE programs SET program_type = 'campus_benefit' WHERE id = $1")
        .bind(campus.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let blocked = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("UPDATE benefit_source_domains SET collection_mode = 'blocked' WHERE id = $1")
        .bind(blocked.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    let human = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query(
        "UPDATE program_source_monitors SET verification_mode = 'human' WHERE candidate_id = $1",
    )
    .bind(human.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let summary = pass(&pool, &[&campus, &blocked, &human], VerifyMode::Shadow, 10).await;
    assert_eq!(
        summary.outcomes.get("campus_out_of_scope"),
        Some(&1),
        "{:?}",
        summary.outcomes
    );
    assert_eq!(summary.outcomes.get("policy_blocked"), Some(&1));
    assert_eq!(summary.outcomes.get("not_evidence_auto"), Some(&1));
    assert_eq!(site.requests(), 0);
    for f in [&campus, &blocked, &human] {
        assert!(proposals(&pool, f).await.is_empty());
        cleanup(&pool, f).await;
    }

    // A script shell is the render lane, never proposed from direct HTML.
    let site = Site::new(
        "<html><body><div id=\"app\">대학생 20% 할인</div><script>boot()</script></body></html>"
            .into(),
    );
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(summary.outcomes.get("js_shell"), Some(&1));
    assert!(proposals(&pool, &f).await.is_empty());
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn the_run_is_capped_and_takes_the_oldest_requests_first() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20, false));
    let mut fixtures = Vec::new();
    for hours in [3, 1, 2] {
        let f = fixture(&pool, &serve(site.clone()).await).await;
        sqlx::query("UPDATE corporate_evidence_targets SET reextract_requested_at = now() - make_interval(hours => $2) WHERE candidate_id = $1")
            .bind(f.candidate_id).bind(hours).execute(&pool).await.unwrap();
        fixtures.push(f);
    }
    let all: Vec<&Fixture> = fixtures.iter().collect();
    let summary = pass(&pool, &all, VerifyMode::Shadow, 2).await;
    assert_eq!((summary.sources, summary.proposals), (2, 2));
    assert!(state(&pool, &fixtures[0]).await.is_some(), "3 h old");
    assert!(state(&pool, &fixtures[1]).await.is_none(), "1 h old waits");
    assert!(state(&pool, &fixtures[2]).await.is_some(), "2 h old");
    let rest = pass(&pool, &all, VerifyMode::Shadow, 2).await;
    assert_eq!(rest.sources, 1);
    for f in &fixtures {
        cleanup(&pool, f).await;
    }
}

#[tokio::test]
async fn reextract_tables_are_service_only() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    for table in ["corporate_reextract_state", "corporate_reextract_proposals"] {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname=$1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(enabled && forced, "{table}");
        let policies: Vec<String> =
            sqlx::query_scalar("SELECT qual FROM pg_policies WHERE tablename=$1")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(policies, vec!["is_service_role()".to_string()], "{table}");
    }
}

/// Review M1: a source whose attempt cannot be saved (here a same-host
/// redirect to a URL longer than the proposal column allows) is recorded as
/// a failure with backoff, and the sources after it still run.
#[tokio::test]
async fn a_source_that_cannot_be_saved_backs_off_and_the_run_continues() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let long = Site::new(offer_page(20, false));
    *long.redirect.lock().unwrap() = Some(format!("/r/{}", "a".repeat(2_100)));
    let broken = fixture(&pool, &serve(long.clone()).await).await;
    let healthy_site = Site::new(offer_page(20, false));
    let healthy = fixture(&pool, &serve(healthy_site.clone()).await).await;
    let before = public_snapshot(&pool, &broken).await;

    let http = LoopbackHttp::new(UA);
    let summary = run_with(
        &pool,
        VerifyMode::Shadow,
        false,
        true,
        &http,
        Some(vec![broken.candidate_id, healthy.candidate_id]),
        10,
        Some(std::time::Duration::from_millis(5)),
        loopback_check,
        None,
    )
    .await
    .expect("one unsavable source never aborts the run");
    assert_eq!(summary.sources, 2);
    assert_eq!(proposals(&pool, &healthy).await.len(), 1);
    assert!(proposals(&pool, &broken).await.is_empty());
    let s = state(&pool, &broken)
        .await
        .expect("the failure is recorded");
    assert_eq!(s.consecutive_failures, 1);
    assert!(s.handled_request_at.is_none());
    assert_eq!(s.last_outcome.as_deref(), Some("record_failed"));
    assert!(s.next_attempt_at - Utc::now() > Duration::hours(5));
    // Inside the backoff the source is not fetched again.
    let hits = long.requests();
    pass(&pool, &[&broken], VerifyMode::Shadow, 10).await;
    assert_eq!(long.requests(), hits);
    assert_eq!(public_snapshot(&pool, &broken).await, before);
    cleanup(&pool, &broken).await;
    cleanup(&pool, &healthy).await;
}

/// Review M2 (rework): a company source without a brand row is read under
/// its corporate programme's provider name, as before.
#[tokio::test]
async fn a_company_without_a_brand_row_is_allowed_under_its_provider_name() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("DELETE FROM benefit_candidate_brands WHERE candidate_id = $1")
        .bind(f.candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(summary.proposals, 1, "{:?}", summary.outcomes);
    assert_eq!(proposals(&pool, &f).await[0].offer["brand"], "픽스처카페");
    cleanup(&pool, &f).await;
}

/// Review M2 (rework): campus scope is refused explicitly, with the rule a
/// new candidate gets: a campus programme, a university domain (`ac.kr`,
/// `.edu`, the `universities` list), whatever name the programme carries.
#[tokio::test]
async fn a_campus_source_without_a_brand_row_is_refused() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20, false));
    let mut fixtures = Vec::new();
    // 1. A campus programme (the provider is the school).
    let programme = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("UPDATE programs SET program_type = 'campus_benefit', provider_name = '픽스처대학교' WHERE id = $1")
        .bind(programme.program_id).execute(&pool).await.unwrap();
    fixtures.push(programme);
    // 2. A corporate-typed programme on an ac.kr domain.
    let ac_kr = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("UPDATE benefit_source_domains SET domain = replace(domain, '.reextract.example', '.ac.kr') WHERE id = $1")
        .bind(ac_kr.domain_id).execute(&pool).await.unwrap();
    fixtures.push(ac_kr);
    // 3. A domain on the university list that is not under ac.kr.
    let listed = fixture(&pool, &serve(site.clone()).await).await;
    let host: String =
        sqlx::query_scalar("SELECT domain FROM benefit_source_domains WHERE id = $1")
            .bind(listed.domain_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let university_id = format!("fixture-{}", &listed.domain_id.simple().to_string()[..12]);
    sqlx::query("INSERT INTO universities(id,name,official_url) VALUES($1,$1,$2)")
        .bind(&university_id)
        .bind(format!("https://www.{host}/"))
        .execute(&pool)
        .await
        .unwrap();
    fixtures.push(listed);
    for f in &fixtures {
        sqlx::query("DELETE FROM benefit_candidate_brands WHERE candidate_id = $1")
            .bind(f.candidate_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let all: Vec<&Fixture> = fixtures.iter().collect();
    let summary = pass(&pool, &all, VerifyMode::Shadow, 10).await;
    sqlx::query("DELETE FROM universities WHERE id = $1")
        .bind(&university_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        summary.outcomes.get("campus_out_of_scope"),
        Some(&3),
        "{:?}",
        summary.outcomes
    );
    assert_eq!(site.requests(), 0, "refused before any request");
    for f in &fixtures {
        assert!(proposals(&pool, f).await.is_empty());
        cleanup(&pool, f).await;
    }
}

/// Review M2 (rework): a source with its own brand row is read as before,
/// under the brand's name.
#[tokio::test]
async fn a_branded_company_is_unchanged() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let brand: String = sqlx::query_scalar("SELECT name FROM brands WHERE id = $1")
        .bind(f.brand_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let summary = pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
    assert_eq!(summary.proposals, 1, "{:?}", summary.outcomes);
    assert_eq!(proposals(&pool, &f).await[0].offer["brand"], brand);
    cleanup(&pool, &f).await;
}

/// Review M3: the failure event resolves once the evidence job no longer
/// asks for fresh extraction (flag cleared, or the request row gone).
#[tokio::test]
async fn the_failure_event_resolves_when_the_request_goes_away() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    for removal in [
        "UPDATE corporate_evidence_targets SET reextract_requested_at = NULL, reextract_reason = NULL WHERE candidate_id = $1",
        "DELETE FROM corporate_evidence_targets WHERE candidate_id = $1",
    ] {
        let site = Site::new(String::new());
        *site.page.lock().unwrap() = (503, "unavailable".into());
        let f = fixture(&pool, &serve(site.clone()).await).await;
        for _ in 0..FAILURE_EVENT_THRESHOLD {
            due_now(&pool, &f).await;
            pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
        }
        assert_eq!(event(&pool, &f).await.unwrap().0, "open");
        sqlx::query(removal)
            .bind(f.candidate_id)
            .execute(&pool)
            .await
            .unwrap();
        let hits = site.requests();
        pass(&pool, &[&f], VerifyMode::Shadow, 10).await;
        assert_eq!(site.requests(), hits, "nothing is fetched for it");
        assert_eq!(event(&pool, &f).await.unwrap().0, "resolved", "{removal}");
        // A later request starts with a clean failure count.
        let s = state(&pool, &f).await.unwrap();
        assert_eq!(s.consecutive_failures, 0);
        cleanup(&pool, &f).await;
    }
}

// ── publish (live only) ──────────────────────────────────────────────────

/// Test double for the whole-page scan (the production parser lives in
/// `resilience`): a page that says 종료 has ended.
fn test_scan(html: &str, _quotes: &[&str]) -> ScanVerdict {
    if html.contains("종료") {
        ScanVerdict::Ended("종료".into())
    } else {
        ScanVerdict::Clean
    }
}

async fn pass_scan(
    pool: &PgPool,
    f: &[&Fixture],
    mode: VerifyMode,
    scan: Option<EndingScan>,
) -> ReextractSummary {
    pass_scan_publishing(pool, f, mode, mode == VerifyMode::Live, scan).await
}

/// `publish`: `CORPORATE_AUTO_PUBLISH_ENABLED` (live only).
async fn pass_scan_publishing(
    pool: &PgPool,
    f: &[&Fixture],
    mode: VerifyMode,
    publish: bool,
    scan: Option<EndingScan>,
) -> ReextractSummary {
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        mode,
        publish,
        true,
        &http,
        Some(f.iter().map(|f| f.candidate_id).collect()),
        10,
        Some(std::time::Duration::from_millis(5)),
        loopback_check,
        scan,
    )
    .await
    .unwrap()
}

async fn shown(pool: &PgPool, f: &Fixture) -> (bool, String, i64, String) {
    let (active, status): (bool, String) =
        sqlx::query_as("SELECT is_active, program_status FROM programs WHERE id = $1")
            .bind(f.program_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_source_holds WHERE program_id = $1 AND resolved_at IS NULL",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let monitor: String = sqlx::query_scalar(
        "SELECT verification_state FROM program_source_monitors WHERE program_id = $1",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (active, status, holds, monitor)
}

#[tokio::test]
async fn shadow_publishes_nothing_even_when_every_gate_passes() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let before = public_snapshot(&pool, &f).await;
    let summary = pass_scan(&pool, &[&f], VerifyMode::Shadow, Some(test_scan)).await;
    assert_eq!((summary.proposals, summary.published), (1, 0));
    let p = &proposals(&pool, &f).await[0];
    assert_eq!(p.status, "proposed");
    assert_eq!(p.gates["ending_scan"], "clean");
    assert!(p.gates.get("publish").is_none());
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

/// §28: live with publishing off records the fresh extraction as a
/// proposal and writes no value: nothing public changes.
#[tokio::test]
async fn live_without_publish_records_the_proposal_and_writes_no_values() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    let before = public_snapshot(&pool, &f).await;
    let summary =
        pass_scan_publishing(&pool, &[&f], VerifyMode::Live, false, Some(test_scan)).await;
    assert_eq!((summary.proposals, summary.published), (1, 0));
    let p = &proposals(&pool, &f).await[0];
    assert_eq!((p.status.as_str(), p.mode.as_str()), ("proposed", "live"));
    assert!(p.gates.get("publish").is_none(), "{:?}", p.gates);
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn live_restores_exactly_one_programme_with_source_evidence() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10, false));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    // Another hidden offer with a flag but outside this pass stays hidden.
    let other = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let active_before: i64 = sqlx::query_scalar("SELECT count(*) FROM programs WHERE is_active")
        .fetch_one(&pool)
        .await
        .unwrap();

    let summary = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(
        (summary.proposals, summary.published),
        (1, 1),
        "{:?}",
        summary.outcomes
    );
    assert_eq!(
        shown(&pool, &f).await,
        (true, "always_open".to_string(), 0, "current".to_string())
    );
    let active_after: i64 = sqlx::query_scalar("SELECT count(*) FROM programs WHERE is_active")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        active_after,
        active_before + 1,
        "exactly one programme restored"
    );
    assert!(!shown(&pool, &other).await.0);

    // The offer is shown on the new quotes from the page; the old fact is
    // retired, not deleted, and remembered so the seed never re-inserts it.
    let facts: Vec<(String, String, String, Value)> = sqlx::query_as(
        "SELECT quote, status, evidence_source, anchors FROM corporate_offer_facts
         WHERE program_id = $1 ORDER BY status, quote",
    )
    .bind(f.program_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let published: Vec<_> = facts.iter().filter(|x| x.1 == "published").collect();
    assert!(!published.is_empty());
    assert!(published.iter().all(|x| x.2 == "reextract"));
    assert!(published.iter().any(|x| x.0.contains("10% 할인")));
    for fact in &published {
        assert!(fact.0.chars().count() <= MAX_QUOTE_CHARS);
        assert!(offer_page(10, false).contains(&fact.0) || fact.0.contains("학생증"));
        assert_eq!(fact.3["previous_quotes"][0], "학생증 제시 시 10% 할인");
    }
    assert!(facts
        .iter()
        .any(|x| x.0 == "학생증 제시 시 10% 할인" && x.1 == "retired"));
    let p = &proposals(&pool, &f).await[0];
    assert_eq!(p.status, "accepted");
    assert_eq!(p.gates["publish"]["outcome"], "restored");
    let check: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM benefit_source_checks WHERE candidate_id = $1
         AND error_summary = 'fresh extraction published'",
    )
    .bind(f.candidate_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(check, 1);
    cleanup(&pool, &f).await;
    cleanup(&pool, &other).await;
}

#[tokio::test]
async fn live_publishes_only_when_the_scan_ran_and_every_shown_value_is_stated() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let refused = |p: &ProposalRow| {
        p.gates["publish"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };

    // No scan available: proposed, never published.
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let before = public_snapshot(&pool, &f).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, None).await;
    assert_eq!((s.proposals, s.published), (1, 0));
    assert_eq!(
        refused(&proposals(&pool, &f).await[0]),
        "ending_scan_unavailable"
    );
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;

    // §25: the page states another value than the one shown: published as
    // the page states it (its own sentence), never as the old title said.
    let f = fixture(&pool, &serve(Site::new(offer_page(20, false))).await).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(s.published, 1, "{:?}", proposals(&pool, &f).await[0].gates);
    let (title, summary): (String, Option<String>) =
        sqlx::query_as("SELECT title, summary FROM programs WHERE id = $1")
            .bind(f.program_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    // §28: the title is the Korean template of the page's facts.
    assert_eq!(title, "픽스처카페 대학생 20% 할인");
    assert!(
        summary
            .as_deref()
            .is_some_and(|s| s.contains("전 메뉴 20% 할인")),
        "{summary:?}"
    );
    cleanup(&pool, &f).await;

    // Normalised value tokens: 1만 원 shown, 10,000원 on the page.
    let f = fixture(
        &pool,
        &serve(Site::new(
            offer_page(10, false).replace("전 메뉴 10% 할인", "전 메뉴 10,000원 할인"),
        ))
        .await,
    )
    .await;
    sqlx::query("UPDATE programs SET summary = '대학생 1만 원 할인' WHERE id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(s.published, 1, "{:?}", proposals(&pool, &f).await[0].gates);
    cleanup(&pool, &f).await;

    // The whole page says the offer ended: nothing is proposed.
    let ended = offer_page(10, false).replace("</main>", "<p>해당 행사는 조기 종료</p></main>");
    let f = fixture(&pool, &serve(Site::new(ended)).await).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(
        s.outcomes.get("offer_ended_scan"),
        Some(&1),
        "{:?}",
        s.outcomes
    );
    assert!(proposals(&pool, &f).await.is_empty());
    assert!(!shown(&pool, &f).await.0);
    cleanup(&pool, &f).await;

    // A publisher opt-out hold is never lifted here.
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    sqlx::query("UPDATE program_source_holds SET hold_reason = 'publisher_optout' WHERE program_id = $1 AND resolved_at IS NULL")
        .bind(f.program_id).execute(&pool).await.unwrap();
    let before = public_snapshot(&pool, &f).await;
    pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(refused(&proposals(&pool, &f).await[0]), "publisher_optout");
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;

    // Two programmes on one source: which one the offer belongs to is not
    // known, so neither is published.
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let second = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                 SELECT $1,program_type,source_type,'fixture:'||$1::text,title||' 2',summary,provider_name,official_url,program_status,provider_kind,benefit_category,false,source_health,regions FROM programs WHERE id=$2")
        .bind(second).bind(f.program_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode)
                 VALUES($1,$2,'review_due',now()-interval '60 days',now()-interval '30 days','evidence_auto')")
        .bind(second).bind(f.candidate_id).execute(&pool).await.unwrap();
    pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(
        refused(&proposals(&pool, &f).await[0]),
        "programmes_on_source:2"
    );
    assert!(!shown(&pool, &f).await.0);
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
}

// ── review of the live publish step (R1–R3) ──────────────────────────────

/// R1: a restore that changes no programme row is not reported, and the
/// publish writes (facts, monitor, candidate, holds) are rolled back with it.
#[tokio::test]
async fn a_restore_that_changes_nothing_is_not_reported_and_leaves_no_trace() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    // Hidden before the hold too: the evidence restore would not show it.
    sqlx::query("UPDATE program_source_holds SET previous_is_active = false WHERE program_id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let before = public_snapshot(&pool, &f).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!((s.proposals, s.published), (1, 0), "{:?}", s.outcomes);
    let p = &proposals(&pool, &f).await[0];
    assert_eq!(p.status, "proposed");
    assert_eq!(p.gates["publish"]["reason"], "restore_not_applied");
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

/// R2: any other programme tied to the source, whatever its verification
/// mode (or with no monitor, by its official link), blocks publishing.
#[tokio::test]
async fn a_source_tied_to_another_programme_in_any_mode_is_not_published() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    for with_monitor in [true, false] {
        let url = serve(Site::new(offer_page(10, false))).await;
        let f = fixture(&pool, &url).await;
        let second = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                     SELECT $1,program_type,source_type,'fixture:'||$1::text,title||' 2',summary,provider_name,official_url,program_status,provider_kind,benefit_category,false,source_health,regions FROM programs WHERE id=$2")
            .bind(second).bind(f.program_id).execute(&pool).await.unwrap();
        if with_monitor {
            // Tied only by its monitor: another link, a human-mode monitor.
            sqlx::query("UPDATE programs SET official_url = 'https://elsewhere.example/other' WHERE id = $1")
                .bind(second).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode)
                         VALUES($1,$2,'review_due',now()-interval '60 days',now()-interval '30 days','human')")
                .bind(second).bind(f.candidate_id).execute(&pool).await.unwrap();
        }
        let before = public_snapshot(&pool, &f).await;
        let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
        assert_eq!(s.published, 0, "with_monitor={with_monitor}");
        assert_eq!(
            proposals(&pool, &f).await[0].gates["publish"]["reason"],
            "programmes_on_source:2",
            "with_monitor={with_monitor}"
        );
        assert_eq!(public_snapshot(&pool, &f).await, before);
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(second)
            .execute(&pool)
            .await
            .unwrap();
        cleanup(&pool, &f).await;
    }
}

/// R3: every public field of the programme must be stated by the page
/// before it is shown again; a field we cannot verify refuses.
#[tokio::test]
async fn every_public_field_must_be_verified_before_publishing() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let with =
        |extra: &str| offer_page(10, false).replace("</section>", &format!("{extra}</section>"));
    let dated = with("<p>행사 기간: 2026.12.31까지</p>");
    let method = with("<p>매장에서 학생증을 보여 주세요.</p>");
    // (page, setup on the programme, expected publish outcome or reason)
    let cases: Vec<(String, &str, &str)> = vec![
        (offer_page(10, false), "UPDATE programs SET application_method = '앱에서 학생 인증 후 사용' WHERE id = $1", "unverified_field:application_method"),
        (method.clone(), "UPDATE programs SET application_method = '매장에서 학생증을 보여 주세요.' WHERE id = $1", "restored"),
        (offer_page(10, false), "UPDATE programs SET submission_documents = '재학증명서' WHERE id = $1", "unverified_field:submission_documents"),
        (offer_page(10, false), "UPDATE programs SET screening_method = '서류 심사' WHERE id = $1", "unverified_field:screening_method"),
        (offer_page(10, false), "UPDATE programs SET application_steps = '[\"앱 설치\"]'::jsonb WHERE id = $1", "unverified_field:application_steps"),
        (offer_page(10, false), "UPDATE programs SET normalized_payload = '{\"conditions\":\"1인 1회\"}'::jsonb WHERE id = $1", "unverified_field:benefit_conditions"),
        (dated.clone(), "UPDATE programs SET benefit_valid_until = '2027-01-31T14:59:59Z' WHERE id = $1", "unverified_field:benefit_valid_until"),
        (dated.clone(), "UPDATE programs SET benefit_valid_until = '2026-12-31T14:59:59Z' WHERE id = $1", "restored"),
        (dated.clone(), "SELECT $1", "unverified_field:end_date_not_shown"),
        (offer_page(10, false), "UPDATE programs SET application_end_at = '2026-12-31T14:59:59Z' WHERE id = $1", "unverified_field:application_end_at"),
        (offer_page(10, false), "UPDATE programs SET application_start_at = '2026-01-01T00:00:00Z' WHERE id = $1", "unverified_field:application_start_at"),
        (offer_page(10, false), "UPDATE programs SET deadline_at = '2026-12-31T14:59:59Z' WHERE id = $1", "unverified_field:deadline_at"),
        (offer_page(10, false), "UPDATE programs SET min_age = 19, max_age = 24 WHERE id = $1", "unverified_field:age"),
        // §25: amounts and the summary are rewritten from the page.
        (offer_page(10, false), "UPDATE programs SET benefit_amount_once = 5000 WHERE id = $1", "restored"),
        (offer_page(10, false), "UPDATE programs SET regions = ARRAY['busan'] WHERE id = $1", "unverified_field:regions"),
        (offer_page(10, false), "UPDATE programs SET region_districts = ARRAY['26110'] WHERE id = $1", "unverified_field:region_districts"),
        (offer_page(10, false), "UPDATE programs SET region_scope = '{\"sido\":\"busan\"}'::jsonb WHERE id = $1", "unverified_field:region_scope"),
        (offer_page(10, false), "UPDATE programs SET school_scope = '[\"부산대학교\"]'::jsonb WHERE id = $1", "unverified_field:school_scope"),
        (offer_page(10, false), "UPDATE programs SET tags = '[\"학생\"]'::jsonb WHERE id = $1", "unverified_field:tags"),
        (offer_page(10, false), "UPDATE programs SET summary = '대학원생 10% 할인' WHERE id = $1", "restored"),
        (offer_page(10, false), "UPDATE programs SET official_url = 'https://other.example/benefit' WHERE id = $1", "unverified_field:official_url"),
    ];
    for (page, setup, expected) in cases {
        let f = fixture(&pool, &serve(Site::new(page)).await).await;
        sqlx::query(setup)
            .bind(f.program_id)
            .execute(&pool)
            .await
            .unwrap();
        let before = public_snapshot(&pool, &f).await;
        let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
        let p = &proposals(&pool, &f).await;
        assert_eq!(p.len(), 1, "{setup}: {:?}", s.outcomes);
        let got = if p[0].gates["publish"]["outcome"] == "restored" {
            "restored".to_string()
        } else {
            p[0].gates["publish"]["reason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(got, expected, "{setup}");
        if expected == "restored" {
            assert_eq!(s.published, 1, "{setup}");
            assert!(shown(&pool, &f).await.0, "{setup}");
        } else {
            assert_eq!(s.published, 0, "{setup}");
            assert_eq!(public_snapshot(&pool, &f).await, before, "{setup}");
        }
        cleanup(&pool, &f).await;
    }
}

/// Review R4: the detail page shows side-table rows nothing here re-checks
/// (application resources, approved AI evidence citations, condition
/// reviews, eligibility rules, required documents). Any such row refuses.
#[tokio::test]
async fn side_table_data_refuses_publishing() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let hash = "a".repeat(64);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("program_application_resources", vec![
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint)
             VALUES($1,'notice','https://notice.example/n','https://notice.example/n','공고문','fixture')".into(),
        ]),
        ("program_ai_extraction_candidates", vec![
            format!("INSERT INTO program_ai_extraction_cache(program_id,source_type,source_hash,provider,model,prompt_version,schema_version,request_hash,source_text,status,failure_count,last_failed_at)
                     VALUES($1,'corporate_official','{hash}','fixture','m','v1',1,'{hash}','본문','failed',1,now())"),
            format!("INSERT INTO program_ai_extraction_candidates(cache_id,program_id,source_hash,provider,model,prompt_version,schema_version,fact_key,fact_type,value,evidence,confidence,status,reviewed_at,review_note)
                     SELECT id,program_id,'{hash}','fixture','m','v1',1,'age','age','{{}}'::jsonb,'{{}}'::jsonb,'HIGH','approved',now(),'ok'
                     FROM program_ai_extraction_cache WHERE program_id=$1"),
        ]),
        ("program_ai_extraction_cache", vec![
            format!("INSERT INTO program_ai_extraction_cache(program_id,source_type,source_hash,provider,model,prompt_version,schema_version,request_hash,source_text,status,failure_count,last_failed_at)
                     VALUES($1,'corporate_official','{hash}','fixture','m','v1',1,'{hash}','본문','failed',1,now())"),
        ]),
        ("program_condition_reviews", vec![
            format!("INSERT INTO program_condition_reviews(source_type,source_id,status,window_json,text_hash,reviewer)
                     SELECT source_type,source_id,'incomplete','{{\"kind\":\"unknown\"}}'::jsonb,'{hash}','fixture'
                     FROM programs WHERE id=$1"),
        ]),
        ("eligibility_rules", vec![
            "INSERT INTO eligibility_rules(program_id,rule_json,explain_json,version) VALUES($1,'{\"all\":[]}'::jsonb,'{}'::jsonb,1)".into(),
        ]),
        ("program_documents", vec![
            "INSERT INTO program_documents(program_id,document_name) VALUES($1,'재학증명서')".into(),
        ]),
    ];
    for (table, setup) in cases {
        let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
        for statement in &setup {
            sqlx::query(statement)
                .bind(f.program_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let before = public_snapshot(&pool, &f).await;
        let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
        assert_eq!(s.published, 0, "{table}");
        let p = &proposals(&pool, &f).await[0];
        assert_eq!(
            p.gates["publish"]["reason"], "unverified_side_data",
            "{table}"
        );
        assert_eq!(p.gates["publish"]["side_tables"][0], table, "{table}");
        assert_eq!(public_snapshot(&pool, &f).await, before, "{table}");
        sqlx::query("DELETE FROM program_condition_reviews WHERE (source_type, source_id) IN (SELECT source_type, source_id FROM programs WHERE id = $1)")
            .bind(f.program_id).execute(&pool).await.unwrap();
        cleanup(&pool, &f).await;
    }
}

// ── contract §16 (round 7) ───────────────────────────────────────────────

/// window_signature, shadow_window_signature, anchors, adopt_before.
type PublishedFactRow = (Option<String>, Option<String>, Value, Option<DateTime<Utc>>);

/// The production scan is the evidence job's `resilience::page_blocked`: a
/// passed end in structured data the visible text never shows still stops
/// the proposal, and a clean page publishes with facts the next evidence
/// run adopts (no window, no shadow window, no fingerprint, short adoption).
#[tokio::test]
async fn the_production_scan_is_page_blocked_and_published_facts_carry_no_baseline() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    assert!(super::super::publish::PAGE_ENDING_SCAN.is_some());

    let ended = offer_page(10, false).replace(
        "</head>",
        "<script type=\"application/ld+json\">{\"@type\":\"Offer\",\"validThrough\":\"2026-01-31\"}</script></head>",
    );
    let f = fixture(&pool, &serve(Site::new(ended)).await).await;
    let s = pass_scan(
        &pool,
        &[&f],
        VerifyMode::Live,
        super::super::publish::PAGE_ENDING_SCAN,
    )
    .await;
    assert_eq!(
        s.outcomes.get("offer_ended_scan"),
        Some(&1),
        "{:?}",
        s.outcomes
    );
    assert!(proposals(&pool, &f).await.is_empty());
    assert!(!shown(&pool, &f).await.0);
    cleanup(&pool, &f).await;

    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let s = pass_scan(
        &pool,
        &[&f],
        VerifyMode::Live,
        super::super::publish::PAGE_ENDING_SCAN,
    )
    .await;
    assert_eq!(s.published, 1, "{:?}", s.outcomes);
    let rows: Vec<PublishedFactRow> = sqlx::query_as(
        "SELECT window_signature, shadow_window_signature, anchors, adopt_before
         FROM corporate_offer_facts WHERE program_id = $1 AND status = 'published'",
    )
    .bind(f.program_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for (window, shadow, anchors, adopt_before) in rows {
        assert!(window.is_none() && shadow.is_none());
        assert!(anchors.get("fingerprint").is_none(), "{anchors}");
        let adopt = adopt_before.expect("adoption window");
        assert!(
            adopt > Utc::now() && adopt <= Utc::now() + Duration::hours(25),
            "{adopt}"
        );
    }
    // The flag stays: the evidence run clears it once the facts confirm.
    assert!(requested_at(&pool, &f).await.is_some());
    cleanup(&pool, &f).await;
}

/// Contract §16: the two value extractors must read the same values on
/// every quote, or nothing is proposed.
#[tokio::test]
async fn values_the_extractors_read_differently_are_never_proposed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    // `25세 미만` is 24 to the age reader and `25세` to the value reader.
    let page = offer_page(10, false).replace(
        "대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
        "대학생(만 19세 이상 25세 미만)은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
    );
    let f = fixture(&pool, &serve(Site::new(page)).await).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(
        s.outcomes.get("extractors_disagree"),
        Some(&1),
        "{:?}",
        s.outcomes
    );
    assert!(proposals(&pool, &f).await.is_empty());
    cleanup(&pool, &f).await;
}

/// `19~24세` is read alike by both extractors (the hand reader reads the
/// range start the value signature leaves out), so it is proposed.
#[tokio::test]
async fn an_age_range_both_extractors_read_alike_is_proposed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let page = offer_page(10, false).replace(
        "대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
        "대학생(19~24세)은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
    );
    let f = fixture(&pool, &serve(Site::new(page)).await).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(
        s.outcomes.get("extractors_disagree"),
        None,
        "{:?}",
        s.outcomes
    );
    assert!(!proposals(&pool, &f).await.is_empty(), "{:?}", s.outcomes);
    cleanup(&pool, &f).await;
}

/// §16b: the scan's named reasons are recorded as the evidence job records
/// them (the exact `resilience` constant, and the `hidden_*` name health
/// reports under `resilience_24h.hidden_by_reason`).
#[tokio::test]
async fn scan_reasons_are_recorded_with_the_evidence_job_names() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    use super::super::resilience::HIDE_REASONS;
    let base = offer_page(10, false);
    // Which reason the reader gives is the evidence job's business (round 9
    // trusts a structured end only for the page's own Offer); each case only
    // needs to be stopped by some named reason, recorded as below.
    let cases = [
        base.replace(
            "</head>",
            "<script type=\"application/ld+json\">{\"@type\":\"Offer\",\"validThrough\":\"2026-01-31\"}</script></head>",
        ),
        base.replace("</section>", "<p>이 혜택은 2099.12.01부터 시작합니다.</p></section>"),
        base.replace("</section>", "<p>준비한 쿠폰이 모두 품절되었습니다.</p></section>"),
    ];
    for page in cases {
        let f = fixture(&pool, &serve(Site::new(page)).await).await;
        let s = pass_scan(
            &pool,
            &[&f],
            VerifyMode::Live,
            super::super::publish::PAGE_ENDING_SCAN,
        )
        .await;
        assert_eq!(
            s.outcomes.get("offer_ended_scan"),
            Some(&1),
            "{:?}",
            s.outcomes
        );
        // The reason is recorded verbatim, and it is one of the evidence
        // job's named reasons.
        let detail: String = sqlx::query_scalar(
            "SELECT last_detail FROM corporate_reextract_state WHERE candidate_id = $1",
        )
        .bind(f.candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(HIDE_REASONS.contains(&detail.as_str()), "{detail}");
        // Its name is health's: `hidden_` + `reason_key`, in the run summary.
        let metric = format!(
            "hidden_{}",
            super::super::resilience::reason_key(&detail).unwrap()
        );
        assert_eq!(
            super::super::publish::hidden_metric(&detail).as_deref(),
            Some(metric.as_str())
        );
        assert_eq!(s.hidden_by_reason.get(&metric), Some(&1), "{metric}");
        assert!(proposals(&pool, &f).await.is_empty());
        cleanup(&pool, &f).await;
    }
}

/// Round 8: `hide_only` never makes anything visible. Fresh extraction in
/// that mode proposes like shadow (the proposal records the mode) and
/// publishes nothing.
#[tokio::test]
async fn hide_only_mode_proposes_and_never_publishes() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let before = public_snapshot(&pool, &f).await;
    let s = pass_scan(&pool, &[&f], VerifyMode::HideOnly, Some(test_scan)).await;
    assert_eq!((s.proposals, s.published), (1, 0), "{:?}", s.outcomes);
    let p = &proposals(&pool, &f).await[0];
    assert_eq!(
        (p.status.as_str(), p.mode.as_str()),
        ("proposed", "hide_only")
    );
    assert!(p.gates.get("publish").is_none());
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

/// §27: a published offer shows the page's whole section (whitespace
/// normalised), a title of brand and audience without a value, and never
/// an amount.
#[tokio::test]
async fn a_published_offer_shows_the_page_sentence_verbatim() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    sqlx::query("UPDATE programs SET benefit_amount_monthly = 3000 WHERE id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let s = pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    assert_eq!(s.published, 1, "{:?}", proposals(&pool, &f).await[0].gates);
    type Shown = (
        String,
        Option<String>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
    );
    let row: Shown = sqlx::query_as(
        "SELECT title, summary, benefit_amount_monthly, benefit_amount_semester, benefit_amount_once,
                normalized_payload->>'display' FROM programs WHERE id = $1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "픽스처카페 대학생 10% 할인");
    // §28: the Korean template, then the fact's own whole sentence (the page
    // has no condition to carry).
    assert_eq!(
        row.1.as_deref(),
        Some("대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.")
    );
    assert_eq!(
        (row.2, row.3, row.4),
        (None, None, None),
        "a corporate offer never carries an amount (the seeded 3000 is cleared)"
    );
    assert_eq!(row.5.as_deref(), Some("verbatim"));
    cleanup(&pool, &f).await;
}

/// Review M3: publishing checks the opt-out record itself, not only a hold.
/// A display-only opt-out (collection goes on) covering an offer that has
/// no opt-out hold (created after the opt-out, say) is never published;
/// neither is one under a URL prefix opt-out. A lifted opt-out no longer
/// blocks.
#[tokio::test]
async fn an_open_optout_covering_the_source_refuses_publishing_without_a_hold() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let refused = |p: &ProposalRow| {
        p.gates["publish"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    for (scope, prefix, lifted) in [
        ("display", false, false),
        ("both", true, false),
        ("collection", false, false),
        ("both", false, true),
    ] {
        let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
        let domain: String =
            sqlx::query_scalar("SELECT domain FROM benefit_source_domains WHERE id=$1")
                .bind(f.domain_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let url: String =
            sqlx::query_scalar("SELECT url FROM benefit_source_candidates WHERE id=$1")
                .bind(f.candidate_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        // The loopback source is plain http; a prefix opt-out is always
        // https, so the prefix case covers the canonical https form.
        if prefix {
            sqlx::query("UPDATE benefit_source_candidates SET canonical_url = replace(url, 'http://', 'https://') WHERE id=$1")
                .bind(f.candidate_id).execute(&pool).await.unwrap();
        }
        let optout: Uuid = sqlx::query_scalar(
            "INSERT INTO benefit_source_optouts(domain, url_prefix, scope, channel, domain_ids, effective_at)
             VALUES(CASE WHEN $4 THEN NULL ELSE $1 END, CASE WHEN $4 THEN replace($2, 'http://', 'https://') END,
                    $3, 'contact_reply', ARRAY[$5]::uuid[], now()) RETURNING id",
        )
        .bind(&domain)
        .bind(&url)
        .bind(scope)
        .bind(prefix)
        .bind(f.domain_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        if lifted {
            sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE id=$1")
                .bind(optout)
                .execute(&pool)
                .await
                .unwrap();
            pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
            let rows = proposals(&pool, &f).await;
            assert_ne!(
                refused(&rows[0]),
                "publisher_optout",
                "a lifted opt-out no longer blocks"
            );
            assert!(shown(&pool, &f).await.0, "published once lifted");
            cleanup(&pool, &f).await;
            continue;
        }
        let before = public_snapshot(&pool, &f).await;
        pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
        let rows = proposals(&pool, &f).await;
        assert!(!rows.is_empty(), "{scope}: the page is read");
        assert_eq!(refused(&rows[0]), "publisher_optout", "{scope}");
        assert!(!shown(&pool, &f).await.0, "{scope}");
        assert_eq!(
            public_snapshot(&pool, &f).await["program"],
            before["program"],
            "{scope}"
        );
        sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE id=$1")
            .bind(optout)
            .execute(&pool)
            .await
            .unwrap();
        cleanup(&pool, &f).await;
    }
}

/// Merge of review M3 with proof-before-effect: an unproven request
/// (`effective_at` NULL) changes nothing, publishing included, or the
/// public form could freeze any source's updates without domain control.
#[tokio::test]
async fn an_unproven_optout_request_does_not_refuse_publishing() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, &serve(Site::new(offer_page(10, false))).await).await;
    let domain: String =
        sqlx::query_scalar("SELECT domain FROM benefit_source_domains WHERE id=$1")
            .bind(f.domain_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let optout: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts(domain, scope, channel, domain_ids)
         VALUES($1, 'both', 'web_form', ARRAY[$2]::uuid[]) RETURNING id",
    )
    .bind(&domain)
    .bind(f.domain_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    pass_scan(&pool, &[&f], VerifyMode::Live, Some(test_scan)).await;
    let rows = proposals(&pool, &f).await;
    assert!(!rows.is_empty(), "the page is read");
    assert_ne!(
        rows[0].gates["publish"]["reason"].as_str().unwrap_or(""),
        "publisher_optout"
    );
    sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE id=$1")
        .bind(optout)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
}
