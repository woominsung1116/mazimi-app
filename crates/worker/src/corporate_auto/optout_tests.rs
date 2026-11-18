use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use std::sync::{Arc, Mutex};

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";

/// Opt-out tests share the global daily cap, so they run one at a time.
static DB_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

#[test]
fn only_registrable_hosts_are_accepted() {
    assert_eq!(
        registrable_host("WWW.Example.co.kr.").as_deref(),
        Some("example.co.kr")
    );
    assert_eq!(
        registrable_host("shop.kt.com").as_deref(),
        Some("shop.kt.com")
    );
    for refused in [
        "co.kr",
        "ac.kr",
        "com",
        "github.io",
        "www.co.kr",
        "127.0.0.1",
        "localhost",
        "example.notarealtld",
        "-bad.com",
        "exa mple.com",
        "a_b.com",
    ] {
        assert!(registrable_host(refused).is_none(), "{refused}");
    }
}

#[test]
fn scope_parsing_is_closed() {
    assert_eq!(Scope::parse("both"), Some(Scope::Both));
    assert!(Scope::parse("everything").is_none());
    assert!(Scope::Collection.stops_collection() && !Scope::Collection.hides_offers());
    assert!(!Scope::Display.stops_collection() && Scope::Display.hides_offers());
}

async fn pool() -> Option<PgPool> {
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
    host: String,
    domain_id: Uuid,
    candidate_id: Uuid,
    program_id: Uuid,
}

/// A visible offer on one of our source domains, already under an open
/// `verification_overdue` hold-free monitor, with stored evidence.
async fn fixture(pool: &PgPool) -> Fixture {
    let tag = Uuid::new_v4().simple().to_string();
    let host = format!("optout-{tag}.com");
    let url = format!("https://www.{host}/student");
    let domain_id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_domains(organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
         VALUES('opt-out fixture',$1,'brand_official','verified','fixture',now(),'pending','manual_review') RETURNING id",
    )
    .bind(format!("www.{host}"))
    .fetch_one(pool)
    .await
    .unwrap();
    let candidate_id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates(domain_id,url,canonical_url,discovered_via,status,candidate_kind,raw_text)
         VALUES($1,$2,$2,'manual','approved','detail','page excerpt') RETURNING id",
    )
    .bind(domain_id)
    .bind(&url)
    .fetch_one(pool)
    .await
    .unwrap();
    let program_id: Uuid = sqlx::query_scalar(
        "INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
         VALUES('corporate_benefit','corporate_official',$1,'옵트아웃 픽스처','학생 10% 할인','픽스처',$2,'always_open','corporate','discount',true,'current',ARRAY[]::text[]) RETURNING id",
    )
    .bind(format!("optout:{tag}"))
    // The programme's own link points elsewhere: matching goes through the
    // monitored source row, never official_url.
    .bind("https://unrelated.example.net/offer")
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verified_at,review_due_at) VALUES($1,$2,now(),now()+interval '30 days')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,quote,quote_key,evidence_source) VALUES($1,'presence','학생 10% 할인 혜택',repeat('a',64),'fixture')")
        .bind(program_id).execute(pool).await.unwrap();
    Fixture {
        host,
        domain_id,
        candidate_id,
        program_id,
    }
}

async fn cleanup(pool: &PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM benefit_source_optouts WHERE $1 = ANY(domain_ids)")
        .bind(f.domain_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(f.program_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(f.domain_id)
        .execute(pool)
        .await
        .unwrap();
}

/// (is_active, program_status, retirement_reason, open hold reason)
async fn program(pool: &PgPool, f: &Fixture) -> (bool, String, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT p.is_active, p.program_status, p.retirement_reason,
                (SELECT h.hold_reason FROM program_source_holds h WHERE h.program_id = p.id AND h.resolved_at IS NULL)
         FROM programs p WHERE p.id = $1",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// (crawl_allowed, collection_mode, compliance_status)
async fn domain(pool: &PgPool, f: &Fixture) -> (bool, String, String) {
    sqlx::query_as("SELECT crawl_allowed, collection_mode, compliance_status FROM benefit_source_domains WHERE id=$1")
        .bind(f.domain_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn submission(f: &Fixture, scope: Scope) -> Submission {
    Submission {
        domain: Some(f.host.clone()),
        url_prefix: None,
        scope,
        contact_hash: None,
    }
}

async fn quotes(pool: &PgPool, f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM corporate_offer_facts WHERE program_id=$1")
        .bind(f.program_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn raw_text(pool: &PgPool, f: &Fixture) -> Option<String> {
    sqlx::query_scalar("SELECT raw_text FROM benefit_source_candidates WHERE id=$1")
        .bind(f.candidate_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn opted_out(pool: &PgPool, f: &Fixture) -> bool {
    let page = Url::parse(&format!("https://www.{}/student", f.host)).unwrap();
    crate::corporate_auto::policy::opted_out(pool, &page)
        .await
        .unwrap()
}

/// Opt-outs of earlier tests must not be fetched against a test's mock.
async fn close_other_optouts(pool: &PgPool) {
    sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE lifted_at IS NULL")
        .execute(pool)
        .await
        .unwrap();
}

/// Submits, serves the issued token at the opt-out proof path and runs one
/// proof pass: the requester has proven control of the domain.
async fn submit_and_prove(pool: &PgPool, f: &Fixture, request: &Submission) -> Submitted {
    close_other_optouts(pool).await;
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let submitted = submit(pool, request).await.unwrap();
    *site.optout.lock().unwrap() = Some(format!("mazimi-optout: {}\n", submitted.token));
    let summary = check(pool, &origin).await;
    assert_eq!(summary.verified, 1, "{:?}", f.host);
    submitted
}

/// Review M22: the form is public and unauthenticated, so a request alone
/// records a token and changes nothing. Otherwise one actor could hide our
/// sources one after another.
#[tokio::test]
async fn intake_records_a_request_and_changes_nothing_before_proof() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool).await;

    let unknown = Submission {
        domain: Some(format!("other-{}", f.host)),
        ..submission(&f, Scope::Both)
    };
    assert!(matches!(
        submit(&pool, &unknown).await,
        Err(SubmitError::UnknownSource)
    ));

    let submitted = submit(&pool, &submission(&f, Scope::Both)).await.unwrap();
    assert_eq!(submitted.token.len(), 64);
    assert_eq!(
        program(&pool, &f).await,
        (true, "always_open".into(), None, None),
        "still shown, nothing held"
    );
    assert_eq!(
        domain(&pool, &f).await,
        (false, "manual_review".into(), "pending".into()),
        "collection settings untouched"
    );
    assert_eq!(quotes(&pool, &f).await, 1, "stored evidence kept");
    assert!(raw_text(&pool, &f).await.is_some());
    assert!(
        !opted_out(&pool, &f).await,
        "an unproven request does not stop collection"
    );
    let (stored_hash, ids, effective): (String, Vec<Uuid>, bool) = sqlx::query_as(
        "SELECT verification_token_hash, domain_ids, effective_at IS NOT NULL FROM benefit_source_optouts WHERE id=$1",
    )
    .bind(submitted.id).fetch_one(&pool).await.unwrap();
    assert_ne!(stored_hash, submitted.token, "token stored only as a hash");
    assert_eq!(ids, vec![f.domain_id]);
    assert!(!effective);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn proof_applies_the_optout_and_makes_it_permanent_without_touching_compliance() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    close_other_optouts(&pool).await;
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let f = fixture(&pool).await;
    // An existing overdue hold: the opt-out takes it over and records it, so
    // a later lift gives that reason back.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'verification_overdue',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    let submitted = submit(&pool, &submission(&f, Scope::Both)).await.unwrap();

    // No proof, or a wrong token: nothing changes.
    *site.optout.lock().unwrap() = Some(format!("token: {}", "0".repeat(64)));
    let summary = check(&pool, &origin).await;
    assert_eq!((summary.checked, summary.verified), (1, 0));
    assert_eq!(program(&pool, &f).await.2, None);
    assert_eq!(
        program(&pool, &f).await.3.as_deref(),
        Some("verification_overdue")
    );
    assert!(!opted_out(&pool, &f).await);

    *site.optout.lock().unwrap() = Some(format!("mazimi-optout: {}\n", submitted.token));
    let summary = check(&pool, &origin).await;
    assert_eq!((summary.verified, summary.programs_retired), (1, 1));
    let (active, status, reason, hold) = program(&pool, &f).await;
    assert_eq!(
        (active, status.as_str(), reason.as_deref(), hold),
        (false, "closed", Some("provider_withdrawn"), None)
    );
    assert_eq!(
        domain(&pool, &f).await,
        (false, "blocked".into(), "pending".into()),
        "collection stopped, compliance untouched"
    );
    assert!(
        opted_out(&pool, &f).await,
        "collection stops through every path"
    );
    assert_eq!(quotes(&pool, &f).await, 0);
    assert!(raw_text(&pool, &f).await.is_none());
    let (effective, withdrawn, state): (bool, i32, Value) = sqlx::query_as(
        "SELECT effective_at IS NOT NULL, programs_withdrawn, previous_state FROM benefit_source_optouts WHERE id=$1",
    )
    .bind(submitted.id).fetch_one(&pool).await.unwrap();
    assert!(effective);
    assert_eq!(withdrawn, 1);
    assert_eq!(
        state["holds"][0]["prior_hold_reason"],
        "verification_overdue"
    );
    assert_eq!(state["domains"][0]["collection_mode"], "manual_review");
    assert!(
        site.hits.lock().unwrap().iter().any(|p| p == "/robots.txt"),
        "robots checked first"
    );

    // A later opt-in with the same token lifts it. The offer comes back only
    // through re-verification: unretired, still hidden, review due now.
    *site.optin.lock().unwrap() = Some(submitted.token.clone());
    let summary = check(&pool, &origin).await;
    assert_eq!(summary.lifted, 1);
    let (active, status, reason, hold) = program(&pool, &f).await;
    assert_eq!(
        (active, status.as_str(), reason, hold.as_deref()),
        (false, "review_required", None, Some("verification_overdue"))
    );
    let monitor: String = sqlx::query_scalar(
        "SELECT verification_state FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(monitor, "review_due");
    assert_eq!(
        domain(&pool, &f).await,
        (false, "manual_review".into(), "pending".into())
    );
    assert!(!opted_out(&pool, &f).await);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn scope_is_honoured() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let display = fixture(&pool).await;
    submit_and_prove(&pool, &display, &submission(&display, Scope::Display)).await;
    assert_eq!(
        program(&pool, &display).await.2.as_deref(),
        Some("provider_withdrawn"),
        "display opt-out withdraws the offer"
    );
    assert_eq!(
        domain(&pool, &display).await.1,
        "manual_review",
        "collection untouched"
    );
    assert!(!opted_out(&pool, &display).await);
    cleanup(&pool, &display).await;

    let collection = fixture(&pool).await;
    submit_and_prove(
        &pool,
        &collection,
        &submission(&collection, Scope::Collection),
    )
    .await;
    assert!(
        program(&pool, &collection).await.0,
        "collection-only does not hide"
    );
    assert_eq!(domain(&pool, &collection).await.1, "blocked");
    cleanup(&pool, &collection).await;

    // A URL prefix hides only offers under it and never blocks the domain.
    let prefixed = fixture(&pool).await;
    let other = Submission {
        domain: None,
        url_prefix: Some(format!("https://www.{}/other/", prefixed.host)),
        scope: Scope::Both,
        contact_hash: None,
    };
    submit_and_prove(&pool, &prefixed, &other).await;
    assert!(program(&pool, &prefixed).await.0);
    assert_eq!(domain(&pool, &prefixed).await.1, "manual_review");
    cleanup(&pool, &prefixed).await;
}

#[tokio::test]
async fn caps_pause_intake_and_alert_an_operator() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
        .bind(PAUSED_ALERT_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();
    let f = fixture(&pool).await;
    for _ in 0..HOURLY_PER_HOST {
        submit(&pool, &submission(&f, Scope::Collection))
            .await
            .unwrap();
    }
    assert!(matches!(
        submit(&pool, &submission(&f, Scope::Collection)).await,
        Err(SubmitError::TooManyForHost)
    ));
    // Review L-2: the cap counts pending requests per proof host, so a
    // flood against one host pauses only that host.
    let other = fixture(&pool).await;
    let filler: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts(domain,scope,channel,received_at)
         SELECT $2, 'collection', 'web_form', now() - interval '2 hours' FROM generate_series(1, $1::int) g RETURNING id",
    )
    .bind(DAILY_CAP as i32)
    .bind(&other.host)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(matches!(
        submit(&pool, &submission(&other, Scope::Both)).await,
        Err(SubmitError::Paused)
    ));
    assert!(program(&pool, &other).await.0);
    let unrelated = fixture(&pool).await;
    submit(&pool, &submission(&unrelated, Scope::Collection))
        .await
        .expect("another host is not paused");
    // Proven or lifted requests no longer count against the host.
    sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE id = ANY($1)")
        .bind(&filler)
        .execute(&pool)
        .await
        .unwrap();
    submit(&pool, &submission(&other, Scope::Collection))
        .await
        .expect("lifted requests do not keep the host paused");
    let (severity, open): (String, bool) = sqlx::query_as(
        "SELECT severity, resolved_at IS NULL FROM operational_events WHERE fingerprint = $1",
    )
    .bind(PAUSED_ALERT_FINGERPRINT)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((severity.as_str(), open), ("warning", true));
    sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
        .bind(PAUSED_ALERT_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_optouts WHERE id = ANY($1)")
        .bind(&filler)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
    cleanup(&pool, &other).await;
    cleanup(&pool, &unrelated).await;
}

/// Review L-1: an unproven token expires after TOKEN_TTL_DAYS, and fresh
/// requests are checked before old ones so a backlog cannot starve them.
#[tokio::test]
async fn unproven_requests_expire_and_fresh_ones_are_checked_first() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    close_other_optouts(&pool).await;
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let f = fixture(&pool).await;
    let submitted = submit(&pool, &submission(&f, Scope::Both)).await.unwrap();
    *site.optout.lock().unwrap() = Some(submitted.token.clone());
    sqlx::query("UPDATE benefit_source_optouts SET received_at = now() - make_interval(days => $2) WHERE id = $1")
        .bind(submitted.id)
        .bind(TOKEN_TTL_DAYS + 1)
        .execute(&pool)
        .await
        .unwrap();
    let summary = check(&pool, &origin).await;
    assert_eq!(summary.checked, 0, "an expired token is no longer checked");
    assert!(program(&pool, &f).await.0, "and serving it changes nothing");

    // PROOF_CHECKS_PER_RUN old requests, due and checked longest ago, and
    // one fresh request checked more recently: the fresh one still runs.
    let old: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts(domain,scope,channel,verification_token_hash,received_at,proof_checked_at)
         SELECT 'stale-' || g || '.example.com', 'collection', 'web_form', repeat('0', 64),
                now() - interval '10 days', now() - interval '3 days'
         FROM generate_series(1, $1::int) g RETURNING id",
    )
    .bind(PROOF_CHECKS_PER_RUN as i32)
    .fetch_all(&pool)
    .await
    .unwrap();
    let fresh: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts(domain,scope,channel,verification_token_hash,proof_checked_at)
         VALUES ('fresh.example.com', 'collection', 'web_form', repeat('1', 64), now() - interval '2 hours')
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let http = LoopbackHttp::new(UA);
    let base = origin.clone();
    let summary = check_proofs_with(
        &pool,
        &http,
        &move |_host: &str| base.clone(),
        Some(std::time::Duration::from_millis(5)),
    )
    .await
    .unwrap();
    assert_eq!(summary.checked as i64, PROOF_CHECKS_PER_RUN);
    let fresh_checked: bool = sqlx::query_scalar(
        "SELECT proof_checked_at > now() - interval '1 minute' FROM benefit_source_optouts WHERE id = $1",
    )
    .bind(fresh)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(fresh_checked, "the fresh request was not starved");
    sqlx::query("DELETE FROM benefit_source_optouts WHERE id = ANY($1) OR id = $2")
        .bind(&old)
        .bind(fresh)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
}

// ── proof of domain control on a mock site ────────────────────────────────

#[derive(Clone, Default)]
struct Site {
    optout: Arc<Mutex<Option<String>>>,
    optin: Arc<Mutex<Option<String>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

async fn serve(site: Site) -> String {
    use axum::{extract::Request, response::IntoResponse, Router};
    let app = Router::new().fallback(move |request: Request| {
        let site = site.clone();
        async move {
            let path = request.uri().path().to_string();
            site.hits.lock().unwrap().push(path.clone());
            let body = match path.as_str() {
                "/robots.txt" => Some("User-agent: *\nAllow: /\n".to_string()),
                OPTOUT_PROOF_PATH => site.optout.lock().unwrap().clone(),
                OPTIN_PROOF_PATH => site.optin.lock().unwrap().clone(),
                _ => None,
            };
            match body {
                Some(body) => (
                    axum::http::StatusCode::OK,
                    [("content-type", "text/plain")],
                    body,
                )
                    .into_response(),
                None => (axum::http::StatusCode::NOT_FOUND, "missing").into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

async fn check(pool: &PgPool, origin: &str) -> ProofSummary {
    sqlx::query("UPDATE benefit_source_optouts SET proof_checked_at = NULL")
        .execute(pool)
        .await
        .unwrap();
    let http = LoopbackHttp::new(UA);
    let origin = origin.to_string();
    check_proofs_with(
        pool,
        &http,
        &move |_host: &str| origin.clone(),
        Some(std::time::Duration::from_millis(5)),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn an_unproven_optout_is_lifted_without_having_changed_anything() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    close_other_optouts(&pool).await;
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let f = fixture(&pool).await;
    // The offer was already held for a content change; an unproven opt-out
    // never takes that hold over.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'content_changed',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    let submitted = submit(&pool, &submission(&f, Scope::Display))
        .await
        .unwrap();
    assert_eq!(
        program(&pool, &f).await.3.as_deref(),
        Some("content_changed")
    );
    *site.optin.lock().unwrap() = Some(submitted.token.clone());
    assert_eq!(check(&pool, &origin).await.lifted, 1);
    let (active, _, reason, hold) = program(&pool, &f).await;
    assert_eq!(
        (active, reason, hold.as_deref()),
        (true, None, Some("content_changed"))
    );
    let state: (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT verified_at, effective_at, lift_proof FROM benefit_source_optouts WHERE id=$1",
    )
    .bind(submitted.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(state.0.is_none() && state.1.is_none());
    assert!(state.2.unwrap().contains("mazimi-optin"));
    cleanup(&pool, &f).await;
}

/// Review LOW: a URL prefix given for the bare host matches sources stored
/// under `www.` (and the other way round).
#[tokio::test]
async fn a_url_prefix_matches_the_host_form_our_sources_use() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool).await;
    let bare = Submission {
        domain: None,
        url_prefix: Some(format!("https://{}/student", f.host)),
        scope: Scope::Display,
        contact_hash: None,
    };
    let submitted = submit_and_prove(&pool, &f, &bare).await;
    let stored: String =
        sqlx::query_scalar("SELECT url_prefix FROM benefit_source_optouts WHERE id=$1")
            .bind(submitted.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, format!("https://www.{}/student", f.host));
    assert_eq!(
        program(&pool, &f).await.2.as_deref(),
        Some("provider_withdrawn")
    );
    cleanup(&pool, &f).await;
}

/// Review MEDIUM: an operator can lift an abusive or mistaken opt-out
/// through the same reversible path; the offer returns only through
/// re-verification.
#[tokio::test]
async fn an_operator_lift_goes_through_the_reversible_path() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool).await;
    let submitted = submit_and_prove(&pool, &f, &submission(&f, Scope::Both)).await;
    let admin = Uuid::new_v4();
    assert!(admin_lift(&pool, submitted.id, admin).await.unwrap());
    assert!(
        !admin_lift(&pool, submitted.id, admin).await.unwrap(),
        "already lifted"
    );
    let (active, _, reason, hold) = program(&pool, &f).await;
    assert_eq!(
        (active, reason, hold.as_deref()),
        (false, None, Some("verification_overdue"))
    );
    assert_eq!(
        domain(&pool, &f).await.1,
        "manual_review",
        "collection settings restored"
    );
    let proof: String =
        sqlx::query_scalar("SELECT lift_proof FROM benefit_source_optouts WHERE id=$1")
            .bind(submitted.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(proof, format!("admin:{admin}"));
    let listed = status(&pool).await.unwrap();
    let row = listed
        .iter()
        .find(|r| r["id"] == json!(submitted.id))
        .unwrap();
    assert_eq!(row["lifted_by"], "operator");
    assert!(row.get("contact_hash").is_none() && row.get("verification_token_hash").is_none());
    cleanup(&pool, &f).await;
}

/// (reason, previous_is_active, previous_program_status) of the open hold.
async fn open_hold(pool: &PgPool, f: &Fixture) -> Option<(String, bool, String)> {
    sqlx::query_as(
        "SELECT hold_reason, previous_is_active, previous_program_status FROM program_source_holds
         WHERE program_id = $1 AND candidate_id = $2 AND resolved_at IS NULL",
    )
    .bind(f.program_id)
    .bind(f.candidate_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Review M1: a permanent opt-out on an offer that was already held for
/// `access_revoked` (and shown before that hold) is lifted back to that
/// same hold: its reason and the pre-hold state it would restore, not an
/// invented `verification_overdue` with the held (hidden) state.
#[tokio::test]
async fn lifting_a_permanent_optout_restores_the_hold_it_took_over() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    sqlx::query("UPDATE benefit_source_optouts SET lifted_at = now() WHERE lifted_at IS NULL")
        .execute(&pool)
        .await
        .unwrap();
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let f = fixture(&pool).await;
    // Shown, then held for revoked access: hidden, pre-hold state kept.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'access_revoked',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE programs SET is_active=false, program_status='review_required' WHERE id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    let submitted = submit(&pool, &submission(&f, Scope::Both)).await.unwrap();
    *site.optout.lock().unwrap() = Some(submitted.token.clone());
    assert_eq!(check(&pool, &origin).await.programs_retired, 1);
    *site.optout.lock().unwrap() = None;
    *site.optin.lock().unwrap() = Some(submitted.token.clone());
    assert_eq!(check(&pool, &origin).await.lifted, 1);
    let (active, status, reason, _) = program(&pool, &f).await;
    assert_eq!(
        (active, status.as_str(), reason),
        (false, "review_required", None)
    );
    assert_eq!(
        open_hold(&pool, &f).await,
        Some(("access_revoked".into(), true, "always_open".into()))
    );
    cleanup(&pool, &f).await;
}

/// Review M2: a programme the evidence job retired (page gone) between the
/// proven effect (its hold placed) and the permanence step is not unretired
/// when the opt-out is lifted; only a retirement the opt-out itself made is
/// undone. The held pair is in `previous_state`, so lift does visit it.
#[tokio::test]
async fn lifting_an_optout_leaves_an_evidence_retirement_alone() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    close_other_optouts(&pool).await;
    let f = fixture(&pool).await;
    let submitted = submit(&pool, &submission(&f, Scope::Display))
        .await
        .unwrap();
    let load = |pool: PgPool, id: Uuid| async move {
        sqlx::query_as::<_, Pending>(
            "SELECT id, domain, url_prefix, scope, domain_ids, verification_token_hash, verified_at,
                    effective_at, previous_state
             FROM benefit_source_optouts WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let mut item = load(pool.clone(), submitted.id).await;
    item.previous_state = apply_effect(&pool, &item).await.unwrap();
    assert_eq!(
        held_pairs(&item.previous_state).len(),
        1,
        "the effect held it"
    );
    assert_eq!(
        program(&pool, &f).await.3.as_deref(),
        Some("publisher_optout")
    );
    // What evidence's retire_program writes for a page gone twice.
    sqlx::query("UPDATE program_source_monitors SET verification_state='retired', last_error='page gone' WHERE program_id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retired_at=now(), retirement_reason='provider_withdrawn' WHERE id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE program_source_holds SET resolved_at=now(), resolution='retired',
                     resolution_note='Automatically retired: the official page returned 404/410 twice at least 24 h apart'
                 WHERE program_id=$1 AND resolved_at IS NULL")
        .bind(f.program_id).execute(&pool).await.unwrap();
    assert_eq!(
        make_permanent(&pool, &item).await.unwrap(),
        0,
        "nothing left to retire"
    );
    assert!(admin_lift(&pool, submitted.id, Uuid::new_v4())
        .await
        .unwrap());
    let (active, status, reason, hold) = program(&pool, &f).await;
    assert_eq!(
        (active, status.as_str(), reason.as_deref(), hold),
        (false, "closed", Some("provider_withdrawn"), None)
    );
    let monitor: String = sqlx::query_scalar(
        "SELECT verification_state FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(monitor, "retired");
    cleanup(&pool, &f).await;
}

/// Review M4: the proven opt-out takes the programme row lock before it
/// touches any hold, monitor or fact, the order `publish` uses (programme
/// first). Otherwise it updates the monitor and then waits for the
/// programme while a publish holds the programme and waits for the
/// monitor: a deadlock. The request itself writes nothing, so the effect
/// applied on proof is what must wait.
#[tokio::test]
async fn the_proven_effect_waits_for_the_programme_lock_before_touching_its_monitor() {
    let Some(pool) = pool().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    close_other_optouts(&pool).await;
    let f = fixture(&pool).await;
    // The offers publish restores are held: an open hold already exists.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'content_changed',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    let site = Site::default();
    let origin = serve(site.clone()).await;
    let submitted = submit(&pool, &submission(&f, Scope::Both)).await.unwrap();
    *site.optout.lock().unwrap() = Some(submitted.token.clone());
    let mut publisher = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM programs WHERE id = $1 FOR UPDATE")
        .bind(f.program_id)
        .execute(&mut *publisher)
        .await
        .unwrap();
    let proof = {
        let pool = pool.clone();
        tokio::spawn(async move { check(&pool, &origin).await.verified })
    };
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let mut probe = pool.begin().await.unwrap();
    let monitor_free = sqlx::query(
        "SELECT 1 FROM program_source_monitors WHERE program_id = $1 FOR UPDATE NOWAIT",
    )
    .bind(f.program_id)
    .execute(&mut *probe)
    .await
    .is_ok();
    probe.rollback().await.unwrap();
    publisher.rollback().await.unwrap();
    let verified = proof.await.unwrap();
    cleanup(&pool, &f).await;
    assert!(
        monitor_free,
        "the proven effect locked the monitor before the programme"
    );
    assert_eq!(verified, 1);
}
