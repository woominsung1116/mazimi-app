use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";
const SPACING: std::time::Duration = std::time::Duration::from_millis(40);

static DB_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn filler() -> String {
    "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12)
}

/// A page that states one student offer of `percent`.
fn offer_page(percent: u32) -> String {
    format!(
        "<html><head><title>카페 학생 혜택</title></head><body><main>\
         <section><h2>학생 혜택</h2><p>대학생은 학생증 제시 시 전 메뉴 {percent}% 할인 혜택을 받을 수 있습니다.</p></section>\
         <p>{}</p></main></body></html>",
        filler()
    )
}

fn programme(title: &str, summary: &str, url: &str) -> AuditedProgramme {
    AuditedProgramme {
        id: Uuid::new_v4(),
        title: title.into(),
        summary: Some(summary.into()),
        official_url: Some(url.into()),
        is_active: true,
        application_start_at: None,
        application_end_at: None,
        deadline_at: None,
        benefit_valid_until: None,
        benefit_amount_monthly: None,
        benefit_amount_semester: None,
        benefit_amount_once: None,
        min_age: None,
        max_age: None,
        candidate_id: None,
    }
}

fn read(percent: u32) -> (Offer, String) {
    let html = offer_page(percent);
    let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let offer = bootstrap::extract_offer(&html, today).expect("offer");
    (offer, super::super::evidence::visible_text(&html))
}

fn disagreeing(checks: &[FieldCheck]) -> Vec<&'static str> {
    checks
        .iter()
        .filter(|c| !c.agrees)
        .map(|c| c.field)
        .collect()
}

// ── pure ─────────────────────────────────────────────────────────────────

const PAGE: &str = "https://cafe.example/student";

#[test]
fn stored_values_the_page_states_agree() {
    let (offer, text) = read(10);
    let p = programme("픽스처 카페 학생 할인", "대학생 10% 할인", PAGE);
    let checks = compare(&p, &offer, &text, PAGE);
    assert_eq!(disagreeing(&checks), Vec::<&str>::new(), "{checks:?}");
    assert_eq!(
        checks.iter().map(|c| c.field).collect::<Vec<_>>(),
        FIELDS.to_vec()
    );
    // The verbatim display (brand + audience word, the page's sentence).
    let verbatim = programme(
        "픽스처카페 학생 혜택",
        "대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
        PAGE,
    );
    assert_eq!(
        disagreeing(&compare(&verbatim, &offer, &text, PAGE)),
        Vec::<&str>::new()
    );
}

#[test]
fn each_field_the_page_contradicts_is_named() {
    let (offer, text) = read(20);
    let p = programme("픽스처 카페 학생 할인", "대학생 10% 할인", PAGE);
    let checks = compare(&p, &offer, &text, PAGE);
    assert_eq!(disagreeing(&checks), vec!["benefit"]);
    let benefit = checks.iter().find(|c| c.field == "benefit").unwrap();
    assert!(benefit.detail.contains("10%"), "{benefit:?}");

    let (offer, text) = read(10);
    let mut p = programme("카페 학생 15% 할인", "청년 10% 할인", PAGE);
    p.min_age = Some(19);
    p.max_age = Some(24);
    p.benefit_valid_until = Some("2026-12-31T00:00:00Z".parse().unwrap());
    p.benefit_amount_once = Some(5_000);
    p.official_url = Some("https://other.example/student".into());
    let checks = compare(&p, &offer, &text, PAGE);
    assert_eq!(
        disagreeing(&checks),
        vec!["title", "audience", "age", "benefit", "dates", "url"],
        "{checks:?}"
    );

    // The same host, another page.
    let mut p = programme("픽스처 카페 학생 할인", "대학생 10% 할인", PAGE);
    p.official_url = Some("https://cafe.example/".into());
    assert_eq!(disagreeing(&compare(&p, &offer, &text, PAGE)), vec!["url"]);
    // A trailing slash, `www.` or a fragment is the same page.
    p.official_url = Some("https://www.cafe.example/student/#top".into());
    assert_eq!(
        disagreeing(&compare(&p, &offer, &text, PAGE)),
        Vec::<&str>::new()
    );
}

#[test]
fn an_end_date_must_match_both_ways() {
    let html = offer_page(10).replace(
        "</section>",
        "<p>행사 기간: 2026.10.01 ~ 2026.12.31</p></section>",
    );
    let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let offer = bootstrap::extract_offer(&html, today).expect("offer");
    assert_eq!(offer.end_date, NaiveDate::from_ymd_opt(2026, 12, 31));
    let text = super::super::evidence::visible_text(&html);
    let mut p = programme("픽스처 카페 학생 할인", "대학생 10% 할인", PAGE);
    let dates = |p: &AuditedProgramme| {
        compare(p, &offer, &text, PAGE)
            .into_iter()
            .find(|c| c.field == "dates")
            .unwrap()
    };
    assert!(!dates(&p).agrees, "page ends, none stored");
    // 2026-12-31 00:00 KST.
    p.application_end_at = Some("2026-12-30T15:00:00Z".parse().unwrap());
    assert!(dates(&p).agrees, "{:?}", dates(&p));
    p.application_end_at = Some("2026-11-30T15:00:00Z".parse().unwrap());
    assert!(!dates(&p).agrees);
}

fn verdict(title: &str, verdict: &str, fields: &[&str], reason: Option<&str>) -> LatestVerdict {
    LatestVerdict {
        program_id: Uuid::new_v4(),
        title: title.into(),
        verdict: verdict.into(),
        fields: fields.iter().map(|f| f.to_string()).collect(),
        reason: reason.map(str::to_string),
        audited_at: Utc::now(),
        visible: verdict != "unreadable",
        previous_read: None,
    }
}

#[test]
fn the_aggregate_and_digest_numbers_are_right() {
    let rows = vec![
        verdict("A 학생 할인", "agrees", &[], None),
        verdict("B 학생 할인", "agrees", &[], None),
        verdict("C 학생 할인", "agrees", &[], None),
        verdict("D 학생 할인", "disagrees", &["benefit", "url"], None),
        verdict("E 학생 할인", "disagrees", &["benefit"], None),
        verdict("F 학생 할인", "unreadable", &[], Some("robots_disallow")),
        verdict("G 학생 할인", "unreadable", &[], Some("robots_disallow")),
        verdict("H 학생 할인", "unreadable", &[], Some("page_unreachable")),
    ];
    let mut rows = rows;
    // D agreed on its previous read: a transition today. E agreed too, but
    // its disagreeing verdict is two days old: no longer new.
    rows[3].previous_read = Some("agrees".into());
    rows[4].previous_read = Some("agrees".into());
    rows[4].audited_at = Utc::now() - chrono::Duration::days(2);
    let scope = Scope {
        in_scope: 10,
        established: 10,
        established_audited: 8,
    };
    let agg = summarize(&rows, scope, Utc::now());
    assert_eq!(agg["visible_disagreements"], 2);
    assert_eq!(agg["new_disagreements"][0]["title"], "D 학생 할인");
    assert_eq!(agg["new_disagreements"].as_array().unwrap().len(), 1);
    assert_eq!(agg["armed"], false, "8 of 10 established audited");
    assert_eq!(agg["programmes_in_scope"], 10);
    assert_eq!(agg["audited"], 8);
    assert_eq!(agg["agrees"], 3);
    assert_eq!(agg["disagrees"], 2);
    assert_eq!(agg["unreadable"], 3);
    assert_eq!(agg["agree_rate"].as_f64(), Some(0.6), "3 of 5 read");
    assert_eq!(agg["coverage"].as_f64(), Some(0.8));
    assert_eq!(
        agg["disagreement_by_field"],
        json!({"title": 0, "audience": 0, "age": 0, "benefit": 2, "dates": 0, "url": 1})
    );
    assert_eq!(
        agg["unreadable_by_reason"],
        json!({"page_unreachable": 1, "robots_disallow": 2})
    );
    let listed: Vec<&str> = agg["disagreeing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap())
        .collect();
    assert_eq!(listed, vec!["D 학생 할인", "E 학생 할인"]);

    let text = digest_text(&agg);
    assert!(
        text.contains(
            "감사 8 / 대상 10 · 일치 3 · 불일치 2 (노출 중 2) · 판독 불가 3 · 일치율 60.0%"
        ),
        "{text}"
    );
    assert!(text.contains("필드별 불일치: benefit 2 · url 1"), "{text}");
    assert!(text.contains("판독 불가 사유: page_unreachable 1 · robots_disallow 2"));
    assert!(text.contains("- 불일치: D 학생 할인 (benefit, url) · 노출 중"));
    assert!(
        text.contains("경보 대기: 첫 주기 진행 중 (8 / 10건 감사)"),
        "{text}"
    );
    assert!(text.contains("새로 불일치 (24시간): D 학생 할인"), "{text}");
    let full = summarize(
        &rows,
        Scope {
            established_audited: 10,
            ..scope
        },
        Utc::now(),
    );
    assert_eq!(full["armed"], true);
    assert!(!digest_text(&full).contains("경보 대기"));

    let empty = summarize(
        &[],
        Scope {
            in_scope: 7,
            ..Scope::default()
        },
        Utc::now(),
    );
    assert_eq!(empty["agree_rate"], Value::Null);
    assert_eq!(empty["armed"], false);
    assert!(digest_text(&empty).contains("최근 8일 감사 없음 (대상 7건)"));
}

#[test]
fn details_are_capped_and_keep_the_comparison() {
    let long = "가".repeat(3_000);
    let details = json!({
        "checks": {"title": {"agrees": true, "detail": long}, "benefit": {"agrees": false, "detail": long}},
        "read": {"facts": [long.clone()]},
        "gates": {"lane": long.clone()},
    });
    let capped = cap_details(details);
    assert!(
        capped.to_string().len() <= MAX_DETAILS_BYTES,
        "{}",
        capped.to_string().len()
    );
    assert_eq!(capped["truncated"], true);
    assert_eq!(capped["checks"]["benefit"]["agrees"], false);
    let small = json!({"checks": {"title": {"agrees": true, "detail": "stated"}}});
    assert_eq!(cap_details(small.clone()), small);
}

// ── database + mock official site ────────────────────────────────────────

#[derive(Clone, Default)]
struct Site {
    robots: Arc<Mutex<(u16, String)>>,
    page: Arc<Mutex<(u16, String)>>,
    hits: Arc<AtomicUsize>,
    times: Arc<Mutex<Vec<Instant>>>,
    user_agents: Arc<Mutex<Vec<String>>>,
}

impl Site {
    fn new(page: String) -> Self {
        let site = Self::default();
        *site.robots.lock().unwrap() = (200, "User-agent: *\nAllow: /\n".into());
        *site.page.lock().unwrap() = (200, page);
        site
    }

    fn hit(&self, headers: &axum::http::HeaderMap) {
        self.hits.fetch_add(1, Ordering::SeqCst);
        self.times.lock().unwrap().push(Instant::now());
        self.user_agents
            .lock()
            .unwrap()
            .push(headers["user-agent"].to_str().unwrap().to_string());
    }

    fn requests(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Serves `/robots.txt` and `/benefit{n}` (one page per source).
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
    let robots_site = site.clone();
    let app = Router::new()
        .route(
            "/robots.txt",
            get(move |headers: HeaderMap| {
                let site = robots_site.clone();
                async move {
                    site.hit(&headers);
                    let (status, body) = site.robots.lock().unwrap().clone();
                    reply(status, "text/plain", body)
                }
            }),
        )
        .fallback(move |request: axum::extract::Request| {
            let site = site.clone();
            async move {
                site.hit(request.headers());
                if request.uri().path().starts_with("/benefit") {
                    let (status, body) = site.page.lock().unwrap().clone();
                    return reply(status, "text/html; charset=utf-8", body);
                }
                reply(404, "text/html; charset=utf-8", "missing".into())
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

/// The loopback site speaks plain http on a random port. The real
/// [`bootstrap::precheck`] still runs, on the https URL the row would have
/// on its own domain.
fn loopback_check(row: &CandidateRow) -> std::result::Result<Url, &'static str> {
    let fetched = Url::parse(&row.url).map_err(|_| "invalid_url")?;
    let mut as_published = row.clone();
    as_published.url = format!("https://{}{}", row.domain, fetched.path());
    bootstrap::precheck(&as_published)?;
    Ok(fetched)
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

struct Fixture {
    candidate_id: Uuid,
    program_id: Uuid,
    domain_id: Uuid,
    brand_id: Uuid,
    url: String,
}

async fn new_domain(pool: &PgPool) -> Uuid {
    let domain_id = Uuid::new_v4();
    sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
                 VALUES($1,'weekly audit fixture',$2,'brand_official','verified','fixture',now(),'pending','manual_review')")
        .bind(domain_id)
        .bind(format!("{}.audit.example", domain_id.simple()))
        .execute(pool)
        .await
        .unwrap();
    domain_id
}

/// A shown corporate offer stating `대학생 10% 할인`, read from `url`.
async fn fixture(pool: &PgPool, url: &str, domain_id: Option<Uuid>) -> Fixture {
    let domain_id = match domain_id {
        Some(id) => id,
        None => new_domain(pool).await,
    };
    let candidate_id = Uuid::new_v4();
    let program_id = Uuid::new_v4();
    let brand_id = Uuid::new_v4();
    sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at)
                 VALUES($1,$2,$3,$3 || '#' || $1::text,'manual','approved','detail',now())")
        .bind(candidate_id).bind(domain_id).bind(url).execute(pool).await.unwrap();
    let brand_name = format!("감사카페-{}", &brand_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO brands(id,name,normalized_name,source_type,source_id) VALUES($1,$2,$2,'fixture',$1::text)")
        .bind(brand_id).bind(&brand_name).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO benefit_candidate_brands(candidate_id,brand_id) VALUES($1,$2)")
        .bind(candidate_id)
        .bind(brand_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                 VALUES($1,'corporate_benefit','corporate_official',$2,'감사 카페 학생 할인','대학생 10% 할인','감사카페',$3,'always_open','corporate','discount',true,'current',ARRAY[]::text[])")
        .bind(program_id).bind(format!("fixture:{program_id}")).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode)
                 VALUES($1,$2,'current',now(),now()+interval '30 days','evidence_auto')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,status)
                 VALUES($1,'presence','{}'::jsonb,'학생증 제시 시 10% 할인',$2,'fixture','published')")
        .bind(program_id).bind(format!("{:064x}", program_id.as_u128()))
        .execute(pool).await.unwrap();
    Fixture {
        candidate_id,
        program_id,
        domain_id,
        brand_id,
        url: url.to_string(),
    }
}

/// Everything public or user-facing about the offer, and every row a
/// hide, publish or proposal would write.
async fn public_snapshot(pool: &PgPool, f: &Fixture) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'program', (SELECT to_jsonb(p) FROM programs p WHERE p.id = $1),
             'monitors', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.candidate_id) FROM program_source_monitors m WHERE m.program_id = $1),
             'holds', (SELECT jsonb_agg(to_jsonb(h) ORDER BY to_jsonb(h)::text) FROM program_source_holds h WHERE h.program_id = $1),
             'facts', (SELECT jsonb_agg(to_jsonb(x) ORDER BY x.id) FROM corporate_offer_facts x WHERE x.program_id = $1),
             'target', (SELECT to_jsonb(t) FROM corporate_evidence_targets t WHERE t.candidate_id = $2),
             'candidate', (SELECT to_jsonb(c) FROM benefit_source_candidates c WHERE c.id = $2),
             'proposals', (SELECT count(*) FROM corporate_reextract_proposals WHERE candidate_id = $2),
             'reextract_state', (SELECT count(*) FROM corporate_reextract_state WHERE candidate_id = $2),
             'events', (SELECT count(*) FROM operational_events WHERE subject_id IN ($1::text, $2::text)),
             'active_programs', (SELECT count(*) FROM programs WHERE is_active))",
    )
    .bind(f.program_id)
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[derive(Debug, FromRow)]
struct VerdictRow {
    mode: String,
    via: String,
    verdict: String,
    fields: Vec<String>,
    reason: Option<String>,
    reason_class: Option<String>,
    program_active: bool,
    details: Value,
}

async fn verdicts(pool: &PgPool, f: &Fixture) -> Vec<VerdictRow> {
    sqlx::query_as(
        "SELECT mode, via, verdict, fields, reason, reason_class, program_active, details
         FROM corporate_weekly_audit_verdicts WHERE program_id = $1 ORDER BY audited_at, id",
    )
    .bind(f.program_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Ages the programme's verdicts past the weekly interval.
async fn a_week_later(pool: &PgPool, f: &Fixture) {
    sqlx::query(
        "UPDATE corporate_weekly_audit_verdicts SET audited_at = audited_at - interval '8 days'
         WHERE program_id = $1",
    )
    .bind(f.program_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn pass(pool: &PgPool, f: &[&Fixture], mode: VerifyMode, budget: Budget) -> AuditSummary {
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        mode,
        true,
        &http,
        Some(f.iter().map(|f| f.program_id).collect()),
        budget,
        Some(SPACING),
        loopback_check,
        super::super::publish::PAGE_ENDING_SCAN,
    )
    .await
    .unwrap()
}

async fn cleanup(pool: &PgPool, fixtures: &[&Fixture]) {
    for f in fixtures {
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
    }
    for f in fixtures {
        sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
            .bind(f.domain_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_disagreeing_offer_is_recorded_and_never_hidden_in_any_mode() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    // The page now states 20%; the stored offer says 10%.
    let site = Site::new(offer_page(20));
    let base = serve(site.clone()).await;
    let f = fixture(&pool, &format!("{base}/benefit"), None).await;
    let before = public_snapshot(&pool, &f).await;

    for (index, mode) in [VerifyMode::Shadow, VerifyMode::HideOnly, VerifyMode::Live]
        .into_iter()
        .enumerate()
    {
        let summary = pass(&pool, &[&f], mode, Budget::default()).await;
        assert_eq!(
            (
                summary.due,
                summary.disagrees,
                summary.agrees,
                summary.unreadable
            ),
            (1, 1, 0, 0),
            "{mode:?}: {summary:?}"
        );
        assert_eq!(summary.by_field.get("benefit"), Some(&1));
        let rows = verdicts(&pool, &f).await;
        assert_eq!(rows.len(), index + 1);
        let row = rows.last().unwrap();
        assert_eq!(
            (row.mode.as_str(), row.via.as_str(), row.verdict.as_str()),
            (mode.as_str(), "audit_fetch", "disagrees")
        );
        assert_eq!(row.fields, vec!["benefit".to_string()]);
        assert!(row.program_active);
        assert_eq!(row.details["checks"]["benefit"]["agrees"], false);
        assert_eq!(row.details["read"]["facts"][0]["value"], 20);
        // Nothing hidden, published, proposed or raised.
        assert_eq!(public_snapshot(&pool, &f).await, before, "{mode:?}");
        // Recorded once a week: an immediate second pass reads nothing.
        let again = pass(&pool, &[&f], mode, Budget::default()).await;
        assert_eq!((again.due, again.requests), (0, 0));
        a_week_later(&pool, &f).await;
    }
    assert!(site.user_agents.lock().unwrap().iter().all(|ua| ua == UA));
    cleanup(&pool, &[&f]).await;
}

#[tokio::test]
async fn an_agreeing_offer_is_recorded() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10));
    let base = serve(site.clone()).await;
    let f = fixture(&pool, &format!("{base}/benefit"), None).await;
    let before = public_snapshot(&pool, &f).await;
    let summary = pass(&pool, &[&f], VerifyMode::Live, Budget::default()).await;
    assert_eq!((summary.agrees, summary.disagrees), (1, 0), "{summary:?}");
    let rows = verdicts(&pool, &f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, "agrees");
    assert!(rows[0].fields.is_empty() && rows[0].reason.is_none());
    for field in FIELDS {
        assert_eq!(rows[0].details["checks"][*field]["agrees"], true, "{field}");
    }
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &[&f]).await;
}

#[tokio::test]
async fn an_unreadable_page_is_recorded_with_its_reason() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10));
    *site.robots.lock().unwrap() = (200, "User-agent: *\nDisallow: /\n".into());
    let base = serve(site.clone()).await;
    let f = fixture(&pool, &format!("{base}/benefit"), None).await;
    // A programme without any source monitor.
    let lone = fixture(&pool, &format!("{base}/benefit-lone"), None).await;
    sqlx::query("DELETE FROM program_source_monitors WHERE program_id = $1")
        .bind(lone.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let before = public_snapshot(&pool, &f).await;
    let summary = pass(&pool, &[&f, &lone], VerifyMode::Live, Budget::default()).await;
    assert_eq!(summary.unreadable, 2, "{summary:?}");
    let row = &verdicts(&pool, &f).await[0];
    assert_eq!(
        (
            row.verdict.as_str(),
            row.reason.as_deref(),
            row.reason_class.as_deref()
        ),
        ("unreadable", Some("robots_disallow"), Some("policy"))
    );
    assert_eq!(
        site.requests(),
        1,
        "robots only; the page is never requested"
    );
    let row = &verdicts(&pool, &lone).await[0];
    assert_eq!(
        (
            row.via.as_str(),
            row.reason.as_deref(),
            row.reason_class.as_deref()
        ),
        ("no_request", Some("no_source_monitor"), Some("no_source"))
    );
    assert_eq!(public_snapshot(&pool, &f).await, before);
    cleanup(&pool, &[&f, &lone]).await;
}

#[tokio::test]
async fn request_bounds_and_domain_pacing_are_respected() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10));
    let base = serve(site.clone()).await;
    // Three sources on one domain.
    let domain = new_domain(&pool).await;
    let mut fixtures = Vec::new();
    for n in 0..3 {
        fixtures.push(fixture(&pool, &format!("{base}/benefit{n}"), Some(domain)).await);
    }
    let all: Vec<&Fixture> = fixtures.iter().collect();

    // The source cap: two of three, the third stays due.
    let summary = pass(
        &pool,
        &all,
        VerifyMode::Shadow,
        Budget {
            max_sources: 2,
            max_requests: 60,
        },
    )
    .await;
    assert_eq!(
        (summary.due, summary.agrees, summary.deferred_budget),
        (3, 2, 1),
        "{summary:?}"
    );
    assert!(summary.requests <= 2 * REQUESTS_PER_SOURCE);
    assert_eq!(summary.requests as usize, site.requests());
    // Robots once for the domain, then one page per source, each spaced.
    let times = site.times.lock().unwrap().clone();
    assert_eq!(times.len(), 3, "robots + 2 pages");
    for pair in times.windows(2) {
        assert!(
            pair[1] - pair[0] >= SPACING - std::time::Duration::from_millis(5),
            "{:?}",
            pair[1] - pair[0]
        );
    }

    // The request budget: one source's worth (5) fits one source only.
    let summary = pass(
        &pool,
        &all,
        VerifyMode::Shadow,
        Budget {
            max_sources: 12,
            max_requests: REQUESTS_PER_SOURCE + 1,
        },
    )
    .await;
    assert_eq!((summary.due, summary.agrees), (1, 1), "{summary:?}");
    assert!(summary.requests <= REQUESTS_PER_SOURCE + 1);

    // Everything audited this week: the next run sends nothing.
    let hits = site.requests();
    let summary = pass(&pool, &all, VerifyMode::Shadow, Budget::default()).await;
    assert_eq!((summary.due, summary.requests), (0, 0));
    assert_eq!(site.requests(), hits);

    // A budget below one source's worst case sends nothing at all.
    for f in &fixtures {
        a_week_later(&pool, f).await;
    }
    let summary = pass(
        &pool,
        &all,
        VerifyMode::Shadow,
        Budget {
            max_sources: 12,
            max_requests: REQUESTS_PER_SOURCE - 1,
        },
    )
    .await;
    assert_eq!(
        (summary.requests, summary.deferred_budget),
        (0, 3),
        "{summary:?}"
    );
    assert_eq!(site.requests(), hits);
    cleanup(&pool, &all).await;
}

#[tokio::test]
async fn a_page_the_evidence_job_just_read_is_not_fetched_again() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20));
    let base = serve(site.clone()).await;
    let f = fixture(&pool, &format!("{base}/benefit"), None).await;
    sqlx::query(
        "INSERT INTO corporate_evidence_targets(candidate_id,last_checked_at,last_outcome,next_check_at)
         VALUES($1, now() - interval '2 hours', 'confirmed', now() + interval '22 hours')",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let before = public_snapshot(&pool, &f).await;

    let summary = pass(&pool, &[&f], VerifyMode::Live, Budget::default()).await;
    assert_eq!(
        (summary.deferred_evidence_recent, summary.requests),
        (1, 0),
        "{summary:?}"
    );
    assert_eq!(site.requests(), 0);
    assert!(verdicts(&pool, &f).await.is_empty());

    // The evidence job's own read is audited in memory, without a request.
    let written = observe_with(
        &pool,
        VerifyMode::Live,
        f.candidate_id,
        &EvidenceRead::Page {
            html: offer_page(20),
            final_url: f.url.clone(),
        },
        loopback_check,
        super::super::publish::PAGE_ENDING_SCAN,
    )
    .await
    .unwrap();
    assert_eq!(written, 1);
    assert_eq!(site.requests(), 0);
    let rows = verdicts(&pool, &f).await;
    assert_eq!(
        (rows[0].via.as_str(), rows[0].verdict.as_str()),
        ("evidence_page", "disagrees")
    );
    assert_eq!(rows[0].fields, vec!["benefit".to_string()]);
    // Not due again this week.
    let again = observe_with(
        &pool,
        VerifyMode::Live,
        f.candidate_id,
        &EvidenceRead::Page {
            html: offer_page(20),
            final_url: f.url.clone(),
        },
        loopback_check,
        None,
    )
    .await
    .unwrap();
    assert_eq!(again, 0);
    assert_eq!(public_snapshot(&pool, &f).await, before);

    // An evidence outcome that sent no request does not defer the audit.
    a_week_later(&pool, &f).await;
    sqlx::query(
        "UPDATE corporate_evidence_targets SET last_outcome = 'opted_out' WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let summary = pass(&pool, &[&f], VerifyMode::Live, Budget::default()).await;
    assert_eq!(
        (summary.deferred_evidence_recent, summary.disagrees),
        (0, 1)
    );
    cleanup(&pool, &[&f]).await;
}

#[tokio::test]
async fn the_aggregate_reads_the_latest_verdict_per_programme() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let agreeing_site = Site::new(offer_page(10));
    let disagreeing_site = Site::new(offer_page(20));
    let a = fixture(
        &pool,
        &format!("{}/benefit", serve(agreeing_site).await),
        None,
    )
    .await;
    let d = fixture(
        &pool,
        &format!("{}/benefit", serve(disagreeing_site).await),
        None,
    )
    .await;
    let ids = [a.program_id, d.program_id];
    pass(&pool, &[&a, &d], VerifyMode::Shadow, Budget::default()).await;
    // An older verdict of the agreeing offer said otherwise; the latest counts.
    sqlx::query(
        "INSERT INTO corporate_weekly_audit_verdicts(program_id,candidate_id,audited_at,mode,via,verdict,fields,program_active)
         VALUES($1,$2,now() - interval '3 days','shadow','audit_fetch','disagrees',ARRAY['title'],true)",
    )
    .bind(a.program_id)
    .bind(a.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let agg = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(
        (
            &agg["programmes_in_scope"],
            &agg["audited"],
            &agg["agrees"],
            &agg["disagrees"]
        ),
        (&json!(2), &json!(2), &json!(1), &json!(1)),
        "{agg}"
    );
    assert_eq!(agg["agree_rate"].as_f64(), Some(0.5));
    assert_eq!(agg["disagreement_by_field"]["benefit"], 1);
    assert_eq!(agg["disagreement_by_field"]["title"], 0);
    assert_eq!(agg["disagreeing"][0]["program_id"], json!(d.program_id));

    // The health row and the digest carry it.
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert!(metrics["weekly_audit"]["disagrees"].as_i64().unwrap() >= 1);
    let digest = crate::digest::build(&pool, Utc::now()).await.unwrap();
    assert!(
        digest.text.contains("■ 기업 혜택 주간 재추출 감사"),
        "{}",
        digest.text
    );
    assert!(digest.text.contains("감사 카페 학생 할인 (benefit)"));
    assert!(
        digest.summary["corporate_weekly_audit"]["disagrees"]
            .as_i64()
            .unwrap()
            >= 1
    );
    cleanup(&pool, &[&a, &d]).await;
}

/// Backdates a fixture programme so it counts as established.
async fn established(pool: &PgPool, fixtures: &[&Fixture]) {
    for f in fixtures {
        sqlx::query("UPDATE programs SET created_at = now() - interval '30 days' WHERE id = $1")
            .bind(f.program_id)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn old_verdict(pool: &PgPool, f: &Fixture, age: &str, verdict: &str, fields: &[&str]) {
    sqlx::query(&format!(
        "INSERT INTO corporate_weekly_audit_verdicts(program_id,candidate_id,audited_at,mode,via,verdict,fields,program_active)
         VALUES($1,$2,now() - interval '{age}','shadow','audit_fetch',$3,$4,true)"
    ))
    .bind(f.program_id)
    .bind(f.candidate_id)
    .bind(verdict)
    .bind(fields.iter().map(|f| f.to_string()).collect::<Vec<_>>())
    .execute(pool)
    .await
    .unwrap();
}

fn alerts_for(agg: &Value) -> super::super::health::Alerts {
    super::super::health::evaluate(&json!({ "weekly_audit": agg }), None, false)
}

#[tokio::test]
async fn alerts_do_not_flap_across_the_cycle_boundary() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20));
    let base = serve(site).await;
    // `standing` disagreed a week ago and still does; `turned` agreed a week
    // ago and now disagrees.
    let standing = fixture(&pool, &format!("{base}/benefit1"), None).await;
    let turned = fixture(&pool, &format!("{base}/benefit2"), None).await;
    established(&pool, &[&standing, &turned]).await;
    old_verdict(&pool, &standing, "15 days", "agrees", &[]).await;
    old_verdict(&pool, &standing, "7 days 1 hour", "disagrees", &["benefit"]).await;
    old_verdict(&pool, &turned, "7 days 1 hour", "agrees", &[]).await;
    let ids = [standing.program_id, turned.program_id];

    // Just past due, before the re-audit: last week's verdicts still count.
    let before = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(before["armed"], true, "{before}");
    assert_eq!(before["visible_disagreements"], 1);
    let alerts = alerts_for(&before);
    assert_eq!(alerts.audit_visible_disagreements, Some(1));
    assert_eq!(alerts.audit_new_disagreements, None);

    // The re-audit: the standing one stays one, the turned one is new.
    let summary = pass(
        &pool,
        &[&standing, &turned],
        VerifyMode::Shadow,
        Budget::default(),
    )
    .await;
    assert_eq!(summary.disagrees, 2, "{summary:?}");
    let after = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(after["visible_disagreements"], 2);
    let alerts = alerts_for(&after);
    assert_eq!(alerts.audit_visible_disagreements, Some(2));
    assert_eq!(
        alerts.audit_new_disagreements,
        Some(vec!["감사 카페 학생 할인".to_string()]),
        "only the programme that agreed before"
    );

    // Every day of the following cycle: the standing warning holds, no new
    // transition, nothing re-fetched.
    for day in 1..=7 {
        sqlx::query(
            "UPDATE corporate_weekly_audit_verdicts SET audited_at = audited_at - interval '1 day'
             WHERE program_id = ANY($1)",
        )
        .bind(&ids[..])
        .execute(&pool)
        .await
        .unwrap();
        if day < 7 {
            let summary = pass(
                &pool,
                &[&standing, &turned],
                VerifyMode::Shadow,
                Budget::default(),
            )
            .await;
            assert_eq!(summary.due, 0, "day {day}");
        } else {
            // Day 7 is due again (two hours of slack): the re-audit lands
            // before the previous verdict leaves the 8-day window.
            let summary = pass(
                &pool,
                &[&standing, &turned],
                VerifyMode::Shadow,
                Budget::default(),
            )
            .await;
            assert_eq!(summary.due, 2, "day {day}");
        }
        let agg = aggregate(&pool, Some(&ids)).await.unwrap();
        let alerts = alerts_for(&agg);
        assert_eq!(
            alerts.audit_visible_disagreements,
            Some(2),
            "day {day}: {agg}"
        );
        assert_eq!(alerts.audit_new_disagreements, None, "day {day}");
    }

    // Hiding the offers (by someone else) clears the standing warning.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(&ids[..])
        .execute(&pool)
        .await
        .unwrap();
    let agg = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(alerts_for(&agg).audit_visible_disagreements, None);
    cleanup(&pool, &[&standing, &turned]).await;
}

#[tokio::test]
async fn the_first_cycle_is_silent() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(20));
    let base = serve(site).await;
    let first = fixture(&pool, &format!("{base}/benefit1"), None).await;
    let second = fixture(&pool, &format!("{base}/benefit2"), None).await;
    established(&pool, &[&first, &second]).await;
    let ids = [first.program_id, second.program_id];
    // The first day of the audit: one of two read, and it disagrees.
    pass(
        &pool,
        &[&first, &second],
        VerifyMode::Shadow,
        Budget {
            max_sources: 1,
            max_requests: 60,
        },
    )
    .await;
    let agg = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(
        (&agg["armed"], &agg["visible_disagreements"]),
        (&json!(false), &json!(1))
    );
    assert_eq!(alerts_for(&agg), super::super::health::Alerts::default());
    // A brand-new programme (not yet established) never holds it back.
    let fresh = fixture(&pool, &format!("{base}/benefit3"), None).await;
    let all = [first.program_id, second.program_id, fresh.program_id];
    // The cycle completes.
    pass(
        &pool,
        &[&first, &second],
        VerifyMode::Shadow,
        Budget::default(),
    )
    .await;
    let agg = aggregate(&pool, Some(&all)).await.unwrap();
    assert_eq!(agg["armed"], true, "{agg}");
    assert_eq!(alerts_for(&agg).audit_visible_disagreements, Some(2));
    cleanup(&pool, &[&first, &second, &fresh]).await;
}

fn injected_panic() -> usize {
    panic!("injected extractor panic")
}

#[tokio::test]
async fn a_panic_in_the_hook_is_isolated_logged_counted_and_recorded() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, "https://hook.audit.example/benefit", None).await;
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = 'corporate_weekly_audit:hook'")
        .execute(&pool)
        .await
        .unwrap();
    let failures = HOOK_FAILURES.load(Ordering::SeqCst);
    let started = Instant::now();
    let handle = isolate(pool.clone(), VerifyMode::Live, f.candidate_id, async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(injected_panic())
    });
    // The caller (the evidence pass) is not held up.
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    handle.await.expect("the supervisor survives the panic");
    assert_eq!(HOOK_FAILURES.load(Ordering::SeqCst), failures + 1);
    let rows = verdicts(&pool, &f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].verdict.as_str(),
            rows[0].reason.as_deref(),
            rows[0].reason_class.as_deref()
        ),
        ("unreadable", Some("audit_failed"), Some("transient"))
    );
    // An error is handled the same way and counted again.
    isolate(pool.clone(), VerifyMode::Live, f.candidate_id, async {
        Err::<usize, _>(anyhow::anyhow!("injected audit error"))
    })
    .await
    .unwrap();
    assert_eq!(HOOK_FAILURES.load(Ordering::SeqCst), failures + 2);
    let occurrences: i32 = sqlx::query_scalar(
        "SELECT occurrence_count FROM operational_events WHERE fingerprint = 'corporate_weekly_audit:hook'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(occurrences, 2);
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = 'corporate_weekly_audit:hook'")
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &[&f]).await;
}

#[tokio::test]
async fn every_programme_gets_a_verdict_each_cycle() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = Site::new(offer_page(10));
    let base = serve(site.clone()).await;
    let fetched = fixture(&pool, &format!("{base}/benefit1"), None).await;
    let evidence_page = fixture(&pool, &format!("{base}/benefit2"), None).await;
    let evidence_gone = fixture(&pool, &format!("{base}/benefit3"), None).await;
    let hook_failed = fixture(&pool, &format!("{base}/benefit4"), None).await;
    let no_monitor = fixture(&pool, &format!("{base}/benefit5"), None).await;
    sqlx::query("DELETE FROM program_source_monitors WHERE program_id = $1")
        .bind(no_monitor.program_id)
        .execute(&pool)
        .await
        .unwrap();
    // The evidence job read three of the sources two hours ago.
    for (f, outcome) in [
        (&evidence_page, "confirmed"),
        (&evidence_gone, "gone"),
        (&hook_failed, "confirmed"),
    ] {
        sqlx::query(
            "INSERT INTO corporate_evidence_targets(candidate_id,last_checked_at,last_outcome)
             VALUES($1, now() - interval '2 hours', $2)",
        )
        .bind(f.candidate_id)
        .bind(outcome)
        .execute(&pool)
        .await
        .unwrap();
    }
    let all = [
        &fetched,
        &evidence_page,
        &evidence_gone,
        &hook_failed,
        &no_monitor,
    ];
    let ids: Vec<Uuid> = all.iter().map(|f| f.program_id).collect();

    let summary = pass(&pool, &all, VerifyMode::Shadow, Budget::default()).await;
    assert_eq!(summary.deferred_evidence_recent, 3, "{summary:?}");
    // What the evidence job handed over: a page, an outcome, a failure.
    observe_with(
        &pool,
        VerifyMode::Shadow,
        evidence_page.candidate_id,
        &EvidenceRead::Page {
            html: offer_page(10),
            final_url: evidence_page.url.clone(),
        },
        loopback_check,
        None,
    )
    .await
    .unwrap();
    observe_with(
        &pool,
        VerifyMode::Shadow,
        evidence_gone.candidate_id,
        &EvidenceRead::Outcome("gone".into()),
        loopback_check,
        None,
    )
    .await
    .unwrap();
    isolate(
        pool.clone(),
        VerifyMode::Shadow,
        hook_failed.candidate_id,
        async { Err::<usize, _>(anyhow::anyhow!("injected")) },
    )
    .await
    .unwrap();

    let agg = aggregate(&pool, Some(&ids)).await.unwrap();
    assert_eq!(
        (&agg["audited"], &agg["programmes_in_scope"]),
        (&json!(5), &json!(5)),
        "{agg}"
    );
    assert_eq!(
        agg["unreadable_by_reason"],
        json!({"audit_failed": 1, "evidence_gone": 1, "no_source_monitor": 1})
    );
    let gone = &verdicts(&pool, &evidence_gone).await[0];
    assert_eq!(
        (gone.via.as_str(), gone.reason_class.as_deref()),
        ("evidence_page", Some("no_offer"))
    );
    // A failed hook is retried after a day, by whichever reads it first.
    sqlx::query(
        "UPDATE corporate_weekly_audit_verdicts SET audited_at = audited_at - interval '25 hours'
         WHERE program_id = $1",
    )
    .bind(hook_failed.program_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE corporate_evidence_targets SET last_checked_at = now() - interval '25 hours'
         WHERE candidate_id = $1",
    )
    .bind(hook_failed.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let summary = pass(&pool, &all, VerifyMode::Shadow, Budget::default()).await;
    assert_eq!((summary.due, summary.agrees), (1, 1), "{summary:?}");
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = 'corporate_weekly_audit:hook'")
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &all).await;
}

#[tokio::test]
async fn a_verdict_that_cannot_be_saved_backs_off_as_record_failed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, "https://save.audit.example/benefit", None).await;
    let mut p = programme("감사 카페 학생 할인", "대학생 10% 할인", &f.url);
    p.id = f.program_id;
    p.candidate_id = Some(f.candidate_id);
    // A page hash the table refuses.
    save_verdict(
        &pool,
        VerifyMode::Shadow,
        &p,
        "audit_fetch",
        &[],
        None,
        Some((&f.url, Some("not-a-hash"))),
        json!({}),
    )
    .await
    .unwrap();
    let rows = verdicts(&pool, &f).await;
    assert_eq!(
        (
            rows[0].verdict.as_str(),
            rows[0].reason.as_deref(),
            rows[0].reason_class.as_deref()
        ),
        ("unreadable", Some("record_failed"), Some("transient"))
    );
    // Not due again until the transient retry.
    assert!(due_programmes(&pool, Some(&[f.program_id]), None)
        .await
        .unwrap()
        .is_empty());
    cleanup(&pool, &[&f]).await;
}

#[tokio::test]
async fn armed_is_sticky_once_any_health_row_was_armed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let day = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
    sqlx::query("DELETE FROM corporate_pipeline_daily WHERE day = $1")
        .bind(day)
        .execute(&pool)
        .await
        .unwrap();
    let before = health_block(&pool).await.unwrap();
    assert!(before["armed_ever"].is_boolean() && before["enabled"].is_boolean());
    sqlx::query("INSERT INTO corporate_pipeline_daily(day, metrics) VALUES($1, $2)")
        .bind(day)
        .bind(json!({"weekly_audit": {"armed": true}}))
        .execute(&pool)
        .await
        .unwrap();
    let block = health_block(&pool).await.unwrap();
    assert_eq!(block["armed_ever"], true, "{block}");
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert_eq!(metrics["weekly_audit"]["armed_ever"], true);
    sqlx::query("DELETE FROM corporate_pipeline_daily WHERE day = $1")
        .bind(day)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_audit_query_degrades_only_its_health_block() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    sqlx::query("ALTER TABLE corporate_weekly_audit_verdicts RENAME TO corporate_weekly_audit_verdicts_away")
        .execute(&pool)
        .await
        .unwrap();
    let metrics = super::super::health::collect(&pool).await;
    sqlx::query("ALTER TABLE corporate_weekly_audit_verdicts_away RENAME TO corporate_weekly_audit_verdicts")
        .execute(&pool)
        .await
        .unwrap();
    let metrics = metrics.expect("the health row is still written");
    assert_eq!(metrics["weekly_audit"], json!({"unavailable": true}));
    assert!(metrics["programs"]["total"].is_number());
}

// ── real pages (2026-10-26): stored offers the page states agree ─────────

#[derive(serde::Deserialize)]
struct RealRow {
    title: String,
    summary: String,
    url: String,
    min_age: Option<i32>,
    file: String,
    sha256: String,
}

fn real_rows() -> Vec<(RealRow, String)> {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audit_20261026");
    let rows: Vec<RealRow> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("index.json")).unwrap()).unwrap();
    rows.into_iter()
        .map(|row| {
            let html = std::fs::read_to_string(dir.join(&row.file)).expect("page");
            use sha2::Digest;
            let digest: String = sha2::Sha256::digest(html.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(digest, row.sha256, "{} unchanged", row.file);
            (row, html)
        })
        .collect()
}

/// The pages the extractor refuses by design, with the reason: Sketch
/// states two terms (one year; for as long as the use is part of the
/// studies), review L2 of #25. The audit reports such a page unreadable,
/// never agreeing.
const REFUSED_BY_DESIGN: &[(&str, &str)] =
    &[("Sketch Education 무료 워크스페이스", "conflicting_terms")];

fn real_checks(row: &RealRow, summary: &str, html: &str) -> Option<Vec<FieldCheck>> {
    let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
    let offer = match bootstrap::extract_offer(html, today) {
        Ok(offer) => offer,
        Err(reason) => {
            assert!(
                REFUSED_BY_DESIGN.contains(&(row.title.as_str(), reason)),
                "{}: {reason}",
                row.title
            );
            return None;
        }
    };
    let mut p = programme(&row.title, summary, &row.url);
    p.min_age = row.min_age;
    Some(compare(
        &p,
        &offer,
        &super::super::evidence::visible_text(html),
        &row.url,
    ))
}

/// The eight visible offers production disagreed with on 2026-10-21: each
/// page states every stored value, in English or Korean prose.
#[test]
fn stored_offers_the_real_pages_state_agree() {
    let mut wrong = Vec::new();
    for (row, html) in real_rows() {
        let Some(checks) = real_checks(&row, &row.summary, &html) else {
            continue;
        };
        let fields = disagreeing(&checks);
        if !fields.is_empty() {
            wrong.push(format!("{}: {checks:?}", row.title));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `summary` with one value changed to one the page does not state: its
/// first number raised until the page states no such value, or, with no
/// number, its claim `무료` replaced by `50% 할인`.
fn one_value_changed(summary: &str, html: &str) -> String {
    let stated = super::super::values::concepts(&super::super::evidence::visible_text(html));
    let Some(found) = regex::Regex::new(r"\d[\d,]*").unwrap().find(summary) else {
        return summary.replacen("무료", "50% 할인", 1);
    };
    let number: u64 = found.as_str().replace(',', "").parse().unwrap();
    (1..100)
        .map(|step| {
            let mut changed = summary.to_string();
            changed.replace_range(found.range(), &(number + step).to_string());
            changed
        })
        .find(|changed| !super::super::values::concepts(changed).is_subset(&stated))
        .expect("a value the page does not state")
}

#[test]
fn a_one_value_mutation_of_each_real_offer_disagrees_on_benefit() {
    for (row, html) in real_rows() {
        let mutated = one_value_changed(&row.summary, &html);
        assert_ne!(mutated, row.summary, "{}", row.title);
        let Some(checks) = real_checks(&row, &mutated, &html) else {
            continue;
        };
        assert!(
            disagreeing(&checks).contains(&"benefit"),
            "{}: {mutated} {checks:?}",
            row.title
        );
    }
}

/// A page the extractor does not read by design (a membership) is
/// `unchecked`, counted apart from pages that could not be read.
#[test]
fn out_of_scope_programmes_are_unchecked_not_unreadable() {
    assert_eq!(reason_class("no_student_youth_offer"), "unchecked");
    assert_eq!(reason_class("robots_disallow"), "policy");
    let rows = vec![
        verdict("멤버십", "unreadable", &[], Some("no_student_youth_offer")),
        verdict("차단", "unreadable", &[], Some("robots_disallow")),
        verdict("학생 할인", "agrees", &[], None),
    ];
    let agg = summarize(
        &rows,
        Scope {
            in_scope: 3,
            established: 3,
            established_audited: 3,
        },
        Utc::now(),
    );
    assert_eq!(
        (agg["unchecked"].as_i64(), agg["unreadable"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(agg["unreadable_by_reason"], json!({"robots_disallow": 1}));
}
