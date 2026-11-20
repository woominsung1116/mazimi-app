//! The door job against a database (`TEST_DATABASE_URL`) and a scripted
//! transport that records every request. Each test makes its own domain
//! and door on an `.example` host and reads only those (`only`).

use super::job::{self, run_with};
use super::lifecycle;
use super::registry::Registry;
use super::{DoorFlags, DoorsMode, Kind};
use crate::corporate_auto::compliance_replay_tests::{reply, ScriptedHttp};
use sqlx::PgPool;
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

const HUBS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/doors_20261110/hubs"
);
const PAGES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/doors_20261110/pages"
);
const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

/// The test database. Without `TEST_DATABASE_URL` a test skips, unless
/// `MAZIMI_REQUIRE_TEST_DB` is set (the local gate sets it): then a
/// missing database fails the test instead of passing it (security review
/// L7).
async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("MAZIMI_REQUIRE_TEST_DB").is_none(),
            "MAZIMI_REQUIRE_TEST_DB is set but TEST_DATABASE_URL is not"
        );
        return None;
    };
    Some(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await
            .unwrap(),
    )
}

fn hub(name: &str) -> String {
    std::fs::read_to_string(format!("{HUBS}/{name}")).unwrap()
}

fn flags(mode: DoorsMode, live: &[Kind]) -> DoorFlags {
    DoorFlags {
        mode,
        live_kinds: live.iter().copied().collect(),
        flags_on: Vec::new(),
        intake_live: true,
    }
}

async fn brand(pool: &PgPool, target_key: &str) -> Uuid {
    sqlx::query_scalar("SELECT brand_id FROM brand_discovery_targets WHERE target_key = $1")
        .bind(target_key)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Removes what earlier runs of these tests left: their doors (on
/// `door-*.example` hosts) and the programmes those doors published, so a
/// shown offer of an earlier run never counts as this run's duplicate.
async fn clean(pool: &PgPool) {
    sqlx::query(
        "DELETE FROM programs WHERE id IN (
             SELECT o.program_id FROM corporate_door_offers o JOIN corporate_doors dr ON dr.id = o.door_id
             JOIN benefit_source_domains d ON d.id = dr.domain_id WHERE d.domain LIKE 'door-%.example')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM corporate_doors WHERE domain_id IN
             (SELECT id FROM benefit_source_domains WHERE domain LIKE 'door-%.example')",
    )
    .execute(pool)
    .await
    .unwrap();
    // The merchant opt-out test's opt-outs would hide its brand's offers in
    // every later run.
    sqlx::query("DELETE FROM benefit_source_optouts WHERE domain LIKE 'merchant-%.example' OR domain LIKE 'gongcha-%.example'")
        .execute(pool)
        .await
        .unwrap();
}

/// A fresh domain and door. `terms`: the domain's terms outcome.
async fn door(
    pool: &PgPool,
    grantor: &str,
    kind: &str,
    terms: Option<&str>,
    path: &str,
) -> (Uuid, String) {
    let host = format!("door-{}.example", Uuid::new_v4().simple());
    let grantor = brand(pool, grantor).await;
    let domain: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_domains (brand_id, organization_name, domain, source_role, crawl_allowed,
                                            verification_status, verification_reference, verified_at,
                                            compliance_status, collection_mode, terms_review_outcome)
         VALUES ($1, 'test', $2, 'benefit_provider', false, 'verified', 'test', now(), 'pending', 'manual_review', $3)
         RETURNING id",
    )
    .bind(grantor)
    .bind(&host)
    .bind(terms)
    .fetch_one(pool)
    .await
    .unwrap();
    let url = format!("https://{host}{path}");
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(domain)
    .bind(&url)
    .bind(grantor)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, url)
}

fn robots_url(page: &str) -> String {
    let url = reqwest::Url::parse(page).unwrap();
    format!("https://{}/robots.txt", url.host_str().unwrap())
}

fn spacing() -> Option<Duration> {
    Some(Duration::from_millis(1))
}

async fn due_now(pool: &PgPool, door: Uuid) {
    sqlx::query("UPDATE corporate_doors SET next_read_at = now() WHERE id = $1")
        .bind(door)
        .execute(pool)
        .await
        .unwrap();
}

/// Ages every offer of the door past the stability wait.
async fn age_offers(pool: &PgPool, door: Uuid) {
    sqlx::query("UPDATE corporate_door_offers SET first_seen_at = now() - interval '25 hours' WHERE door_id = $1")
        .bind(door)
        .execute(pool)
        .await
        .unwrap();
}

/// Reads the door again until a read publishes nothing more (the per-run
/// publish cap is five); returns what was published in all.
async fn publish_all(pool: &PgPool, flags: &DoorFlags, http: &ScriptedHttp, door: Uuid) -> usize {
    let mut total = 0;
    for _ in 0..6 {
        due_now(pool, door).await;
        let run = run_with(pool, flags, http, spacing(), Some(&[door]))
            .await
            .unwrap();
        total += run.published;
        if run.published == 0 {
            break;
        }
    }
    total
}

/// Off: no query of the doors, no request, whatever is due.
#[tokio::test]
async fn off_mode_reads_nothing() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/list",
    )
    .await;
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (
            url.as_str(),
            reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
        ),
    ]);
    let summary = run_with(
        &pool,
        &flags(DoorsMode::Off, &[]),
        &http,
        spacing(),
        Some(&[id]),
    )
    .await
    .unwrap();
    assert_eq!(summary.doors, 0);
    assert!(http.requested().is_empty());
}

/// Review C3: a hub is read only once a person recorded its terms as
/// allowing it. No outcome, `restricted` or `not_found`: no request at all.
#[tokio::test]
async fn a_hub_without_an_allowing_terms_outcome_is_never_read() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    for terms in [None, Some("restricted"), Some("not_found")] {
        let (id, url) = door(&pool, "skt", "hub_rows", terms, "/list").await;
        let http = ScriptedHttp::new(vec![
            (
                robots_url(&url).as_str(),
                reply(200, "text/plain", ALLOW_ALL),
            ),
            (
                url.as_str(),
                reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
            ),
        ]);
        let summary = run_with(
            &pool,
            &flags(DoorsMode::Shadow, &[]),
            &http,
            spacing(),
            Some(&[id]),
        )
        .await
        .unwrap();
        assert_eq!(summary.doors, 0, "{terms:?}");
        assert!(http.requested().is_empty(), "{terms:?}");
    }
    // Of the seeded hubs of 20261111000105, only the two whose terms
    // 20261118000001 recorded are due; PAYCO and the rest carry no outcome.
    let seeded: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT d.domain FROM corporate_doors dr JOIN benefit_source_domains d ON d.id = dr.domain_id
         WHERE dr.kind = 'hub_rows' AND d.domain NOT LIKE '%.example' AND {}
         ORDER BY d.domain",
        super::HUB_TERMS_ALLOWED_SQL
    ))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(seeded, ["card.kbcard.com", "sktmembership.tworld.co.kr"]);
}

/// Shadow: the hub is read once, its offers recorded, nothing published,
/// and it is not read again within the day.
#[tokio::test]
async fn a_shadow_hub_read_records_offers_and_publishes_nothing() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/list",
    )
    .await;
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (
            url.as_str(),
            reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
        ),
    ]);
    let shadow = flags(DoorsMode::Shadow, &[Kind::HubRows]);
    let summary = run_with(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(summary.doors, 1);
    assert_eq!(http.requested(), vec![robots_url(&url), url.clone()]);
    let offers: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT b.name, o.value, o.tiers FROM corporate_door_offers o JOIN brands b ON b.id = o.redeem_brand_id
         WHERE o.door_id = $1 ORDER BY b.name",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<&str> = offers.iter().map(|o| o.0.as_str()).collect();
    assert!(
        names.contains(&"공차") && names.contains(&"CGV"),
        "{offers:?}"
    );
    let (verdict, rows_seen, by_reason): (String, i32, serde_json::Value) = sqlx::query_as(
        "SELECT verdict, rows_seen, by_reason FROM corporate_door_reads WHERE door_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(verdict, "listing");
    assert!(rows_seen >= 15);
    assert!(by_reason.get("rate_per_amount").is_some(), "{by_reason}");
    let programmes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM programs WHERE source_id LIKE $1")
            .bind(format!("door:{id}:%"))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(programmes, 0, "shadow never publishes");
    // Not due again today: a second run makes no request.
    let again = run_with(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(again.doors, 0);
    assert_eq!(http.requested().len(), 2);
}

/// Review H3 and C2: a live hub offer is its grantor's — sponsor, provider
/// and company — redeemable at the brand, with the holder template and the
/// row's restriction as its condition; shown only after two reads a day
/// apart; the requirement reaches current_programs; no logo, no affiliate
/// parameter (M8).
#[tokio::test]
async fn hub_offer_written_as_sponsor_and_redeem() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    clean(&pool).await;
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/list",
    )
    .await;
    let html = hub("sktmembership_list-tab1.html");
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (url.as_str(), reply(200, "text/html", &html)),
    ]);
    let live = flags(DoorsMode::Live, &[Kind::HubRows]);
    let first = run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(first.published, 0, "one read is not stable");
    age_offers(&pool, id).await;
    due_now(&pool, id).await;
    let second = run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert!(
        second.published >= 1 && second.published <= 5,
        "the cap: {second:?}"
    );
    publish_all(&pool, &live, &http, id).await;
    let gongcha = brand(&pool, "gongcha").await;
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Uuid,
        String,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT p.id, p.title, p.provider_name, p.company_name, p.company_logo_url, p.official_url,
                p.eligibility_condition, p.source_health
         FROM corporate_door_offers o JOIN programs p ON p.id = o.program_id
         WHERE o.door_id = $1 AND o.redeem_brand_id = $2",
    )
    .bind(id)
    .bind(gongcha)
    .fetch_optional(&pool)
    .await
    .unwrap();
    let Some((program, title, provider, company, logo, official, condition, health)) = row else {
        panic!("공차 was never published: {second:?}");
    };
    // The row line (review H-2): the membership type stays with the value.
    assert_eq!(title, "SK텔레콤 · 공차 할인형 10% 할인");
    assert_eq!(
        (provider.as_str(), company.as_str()),
        ("SK텔레콤", "SK텔레콤")
    );
    assert_eq!(logo, None);
    assert_eq!(official, url);
    assert!(!official.contains('?'), "no affiliate parameter");
    assert_eq!(condition.as_deref(), Some("SK텔레콤 휴대폰 이용 고객"));
    assert_eq!(health, "current");
    let links: Vec<String> = sqlx::query_scalar(
        "SELECT b.name || ':' || pb.relationship_type FROM program_brands pb JOIN brands b ON b.id = pb.brand_id
         WHERE pb.program_id = $1 ORDER BY 1",
    )
    .bind(program)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(links, ["SK텔레콤:sponsor", "공차:redeemable_at"]);
    let (holder, mode): (Option<String>, String) = sqlx::query_as(
        "SELECT cp.holder_requirement, m.verification_mode FROM current_programs cp
         JOIN program_source_monitors m ON m.program_id = cp.id WHERE cp.id = $1",
    )
    .bind(program)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(holder.as_deref(), Some("telecom_carrier:skt"));
    assert_eq!(mode, "door");

    // The row leaves the hub: the next complete read hides it at once.
    let without = html.replacen(
        "<span class='brand'>공차</span>",
        "<span class='brand'>공차X</span>",
        1,
    );
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (url.as_str(), reply(200, "text/html", &without)),
    ]);
    due_now(&pool, id).await;
    let third = run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert!(third.hidden >= 1, "{third:?}");
    let shown: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM current_programs WHERE id = $1)")
            .bind(program)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!shown);
}

/// Review H8: the same row with a later end date is the same offer; its
/// programme's end moves with it.
#[tokio::test]
async fn an_identical_row_with_a_new_period_continues_the_same_offer() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    clean(&pool).await;
    let (id, url) = door(
        &pool,
        "kbcard",
        "hub_rows",
        Some("no_automated_access_restriction"),
        "/brand",
    )
    .await;
    let html = hub("kbcard_brand_discounts.html");
    let serve = |html: &str| {
        ScriptedHttp::new(vec![
            (
                robots_url(&url).as_str(),
                reply(200, "text/plain", ALLOW_ALL),
            ),
            (url.as_str(), reply(200, "text/html", html)),
        ])
    };
    let live = flags(DoorsMode::Live, &[Kind::HubRows]);
    run_with(&pool, &live, &serve(&html), spacing(), Some(&[id]))
        .await
        .unwrap();
    age_offers(&pool, id).await;
    publish_all(&pool, &live, &serve(&html), id).await;
    let coupang = brand(&pool, "coupang").await;
    let program = |pool: PgPool| async move {
        sqlx::query_as::<_, (Option<Uuid>, String)>(
            "SELECT program_id, status FROM corporate_door_offers WHERE door_id = $1 AND redeem_brand_id = $2",
        )
        .bind(id)
        .bind(coupang)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let (first, status) = program(pool.clone()).await;
    assert_eq!(status, "published");
    let extended = html.replace("2026.11.01 ~ 2026.11.30", "2026.11.01 ~ 2026.12.31");
    due_now(&pool, id).await;
    run_with(&pool, &live, &serve(&extended), spacing(), Some(&[id]))
        .await
        .unwrap();
    let (again, status) = program(pool.clone()).await;
    assert_eq!((again, status.as_str()), (first, "published"));
    let until: String = sqlx::query_scalar(
        "SELECT to_char(benefit_valid_until AT TIME ZONE 'Asia/Seoul', 'YYYY-MM-DD') FROM programs WHERE id = $1",
    )
    .bind(first)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(until, "2026-12-31");
}

/// Review §4.5: a display opt-out of the redeem brand's own domain hides
/// the hub offer that names it.
#[tokio::test]
async fn merchant_optout_hides_hub_offer() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    clean(&pool).await;
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/list",
    )
    .await;
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (
            url.as_str(),
            reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
        ),
    ]);
    let live = flags(DoorsMode::Live, &[Kind::HubRows]);
    run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    age_offers(&pool, id).await;
    due_now(&pool, id).await;
    run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    let published: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT redeem_brand_id, program_id FROM corporate_door_offers WHERE door_id = $1 AND status = 'published'",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let (merchant, program) = *published.first().expect("something published");
    let host = format!("merchant-{}.example", Uuid::new_v4().simple());
    let domain: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_domains (brand_id, organization_name, domain, source_role, crawl_allowed,
                                            verification_status, compliance_status, collection_mode)
         VALUES ($1, 'merchant', $2, 'brand_official', false, 'verified', 'pending', 'manual_review') RETURNING id",
    )
    .bind(merchant)
    .bind(&host)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO benefit_source_domain_brands (domain_id, brand_id) VALUES ($1, $2)")
        .bind(domain)
        .bind(merchant)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO benefit_source_optouts (domain, scope, channel, effective_at, domain_ids)
         VALUES ($1, 'display', 'web_form', now(), ARRAY[$2]::uuid[])",
    )
    .bind(&host)
    .bind(domain)
    .execute(&pool)
    .await
    .unwrap();
    due_now(&pool, id).await;
    let after = run_with(&pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert!(after.hidden >= 1, "{after:?}");
    let active: bool = sqlx::query_scalar("SELECT is_active FROM programs WHERE id = $1")
        .bind(program)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!active);
}

/// Review M6: two gone reads an hour apart lose the door, with an event; a
/// robots disallow loses it at once.
#[tokio::test]
async fn door_lost_after_two_gone_reads() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(&pool, "gongcha", "page", None, "/benefit").await;
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (url.as_str(), reply(404, "text/html", "")),
    ]);
    let shadow = flags(DoorsMode::Shadow, &[]);
    run_with(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM corporate_doors WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "active", "one gone read is not enough");
    sqlx::query("UPDATE corporate_doors SET first_gone_at = now() - interval '2 hours', next_read_at = now() WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    run_with(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    let (state, outcome): (String, String) =
        sqlx::query_as("SELECT state, last_outcome FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), outcome.as_str()), ("lost", "page_gone"));
    let event: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM operational_events WHERE fingerprint = $1)",
    )
    .bind(format!("corporate_door_lost:{id}"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(event);

    let (id, url) = door(&pool, "gongcha", "page", None, "/benefit").await;
    let http = ScriptedHttp::new(vec![(
        robots_url(&url).as_str(),
        reply(200, "text/plain", "User-agent: *\nDisallow: /benefit\n"),
    )]);
    run_with(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM corporate_doors WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "lost");
    assert_eq!(
        http.requested(),
        vec![robots_url(&url)],
        "the page is never requested"
    );
}

/// Review M6: a lost hub is replaced only by a page on its host that reads
/// at least half of its last brands; a page with other brands is not it.
#[tokio::test]
async fn rediscovery_requires_brand_overlap() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/old/list",
    )
    .await;
    let host = reqwest::Url::parse(&url)
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let registry = Registry::load(&pool).await.unwrap();
    let skt = brand(&pool, "skt").await;
    let today = crate::corporate_auto::health::kst_today();
    let last: Vec<Uuid> =
        super::rows::read_hub(&hub("sktmembership_list-tab1.html"), &registry, skt, today)
            .matched_brands()
            .into_iter()
            .collect();
    assert!(last.len() >= 10, "{}", last.len());
    sqlx::query(
        "UPDATE corporate_doors SET state = 'lost', lost_since = now(), last_matched_brands = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(&last)
    .execute(&pool)
    .await
    .unwrap();
    let other = format!("https://{host}/old/other");
    let moved = format!("https://{host}/new/list");
    let sitemap = format!(
        "<?xml version=\"1.0\"?><urlset><url><loc>{other}</loc></url><url><loc>{moved}</loc></url></urlset>"
    );
    let robots = format!("User-agent: *\nAllow: /\nSitemap: https://{host}/sm.xml\n");
    let http = ScriptedHttp::new(vec![
        (
            format!("https://{host}/robots.txt").as_str(),
            reply(200, "text/plain", &robots),
        ),
        (
            format!("https://{host}/sm.xml").as_str(),
            reply(200, "text/plain", &sitemap),
        ),
        (
            format!("https://{host}/sitemap.xml").as_str(),
            reply(404, "text/plain", ""),
        ),
        (
            other.as_str(),
            reply(200, "text/html", &hub("kbcard_brand_discounts.html")),
        ),
        (
            moved.as_str(),
            reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
        ),
    ]);
    let shadow = flags(DoorsMode::Shadow, &[]);
    let replaced = lifecycle::rediscover(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(replaced, 1);
    let (state, replaced_by): (String, Option<Uuid>) =
        sqlx::query_as("SELECT state, replaced_by FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "retired");
    let new_url: String = sqlx::query_scalar("SELECT entry_url FROM corporate_doors WHERE id = $1")
        .bind(replaced_by.unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        new_url, moved,
        "the KB page shares no brand set with the lost hub"
    );
    assert!(
        http.requested().contains(&other),
        "the closer URL was tried first"
    );
}

/// Thirty days lost with no replacement is a counted outcome.
#[tokio::test]
async fn a_door_lost_thirty_days_is_retired_as_unreplaced() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, _) = door(&pool, "gongcha", "page", None, "/benefit").await;
    sqlx::query("UPDATE corporate_doors SET state = 'lost', lost_since = now() - interval '31 days' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    lifecycle::retire_unreplaced(&pool).await.unwrap();
    let (state, outcome): (String, String) =
        sqlx::query_as("SELECT state, last_outcome FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), outcome.as_str()),
        ("retired", "lost_no_replacement_30d")
    );
    let metrics = super::metrics::collect(&pool, 14).await.unwrap();
    assert!(metrics.lost_no_replacement >= 1);
}

/// Deploy report §9: a door whose entry URL already exists as another
/// lane's candidate (the KB hub: a `search_api` candidate in `review`)
/// adopts it in the state the fail-closed overdue hold relies on
/// (`approved`, an index), and a door already linked to a candidate in
/// another state brings it back to that state. Provenance is kept.
#[tokio::test]
async fn a_door_adopting_an_existing_candidate_leaves_it_approved() {
    let Some(pool) = db().await else {
        panic!("TEST_DATABASE_URL required");
    };
    let (id, url) = door(&pool, "gongcha", "hub_rows", None, "/benefit/hub").await;
    let domain: Uuid = sqlx::query_scalar("SELECT domain_id FROM corporate_doors WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let existing: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, status,
                                                candidate_kind, next_check_at)
         VALUES ($1, $2, $2, 'search_api', 'review', 'detail', now() + interval '3 days') RETURNING id",
    )
    .bind(domain)
    .bind(&url)
    .fetch_one(&pool)
    .await
    .unwrap();
    let load = || async {
        sqlx::query_as::<_, job::DoorRow>(
            "SELECT dr.id, dr.domain_id, d.domain, dr.entry_url, dr.candidate_id,
                    dr.grantor_brand_id, b.name AS grantor_name, dr.kind, dr.kind AS lane,
                    dr.parent_door_id, dr.max_bytes, dr.state, dr.first_gone_at, dr.unrecognised_days,
                    dr.consecutive_failures, dr.last_matched_brands, d.gray_robots_missing,
                    d.gray_crawl_delay_min, d.min_request_interval_ms
             FROM corporate_doors dr JOIN benefit_source_domains d ON d.id = dr.domain_id
             JOIN brands b ON b.id = dr.grantor_brand_id WHERE dr.id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let state = || async {
        sqlx::query_as::<_, (String, String, String, bool)>(
            "SELECT status, candidate_kind, discovered_via, next_check_at IS NULL
             FROM benefit_source_candidates WHERE id = $1",
        )
        .bind(existing)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let mut tx = pool.begin().await.unwrap();
    let adopted = lifecycle::door_candidate(&mut tx, &load().await)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(adopted, existing);
    assert_eq!(
        state().await,
        ("approved".into(), "index".into(), "search_api".into(), true)
    );
    let linked: Option<Uuid> =
        sqlx::query_scalar("SELECT candidate_id FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked, Some(existing));
    // Moved out of `approved` by another lane later: the linked door brings
    // it back.
    sqlx::query("UPDATE benefit_source_candidates SET status = 'review' WHERE id = $1")
        .bind(existing)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        lifecycle::door_candidate(&mut tx, &load().await)
            .await
            .unwrap(),
        existing
    );
    tx.commit().await.unwrap();
    assert_eq!(state().await.0, "approved");
}

/// Review H5: a door's candidate is read under the door's brand scope by
/// every reader; under it the 공차 page reads its T 멤버십 discount.
#[tokio::test]
async fn a_door_candidate_is_read_under_its_brand_scope() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(
        &pool,
        "gongcha",
        "page",
        None,
        "/brand/store/skt_membership",
    )
    .await;
    let domain: Uuid = sqlx::query_scalar("SELECT domain_id FROM corporate_doors WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let candidate: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, status)
         VALUES ($1, $2, $2, 'official_page', 'queued') RETURNING id",
    )
    .bind(domain)
    .bind(&url)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE corporate_doors SET candidate_id = $2 WHERE id = $1")
        .bind(id)
        .bind(candidate)
        .execute(&pool)
        .await
        .unwrap();
    let scopes = job::scopes_for(&pool, &[candidate]).await.unwrap();
    let scope = scopes
        .get(&candidate)
        .expect("a door's candidate has a brand scope")
        .clone();
    let html =
        std::fs::read_to_string(format!("{PAGES}/gong-cha.co.kr_skt_membership.html")).unwrap();
    let mut decision = crate::corporate_auto::bootstrap::Decision {
        candidate_ids: vec![candidate.to_string()],
        url: url.clone(),
        final_url: None,
        domain: String::new(),
        brand: Some("공차".into()),
        brand_id: None,
        domain_id: None,
        outcome: String::new(),
        http_status: None,
        page_title: None,
        offer: None,
        visible_chars: None,
        page_hash: None,
        page_text: None,
        page_html: None,
        scope,
    };
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
    crate::corporate_auto::bootstrap::read_page(&mut decision, &html, today);
    assert_eq!(decision.outcome, "proposal");
    let offer = decision.offer.unwrap();
    assert_eq!(offer.issuer.as_ref().unwrap().name, "SK텔레콤");
    assert_eq!(
        crate::corporate_auto::publish::offer_title("공차", &offer).unwrap_or_default(),
        "SK텔레콤 · 공차 상시 혜택 10% 할인",
        "the fact's own sentence (code review H-6)"
    );
    // A candidate no door owns keeps the audience scope.
    assert!(job::scopes_for(&pool, &[Uuid::new_v4()])
        .await
        .unwrap()
        .is_empty());
}

/// The registry the fixture tests use is the migration's (names agree),
/// and no two brands share a name (review H2's loader test).
#[tokio::test]
async fn must_have_aliases_are_unique() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let registry = Registry::load(&pool).await.unwrap();
    assert_eq!(registry.shared_names(), Vec::new());
    let must_have = registry.brands().iter().filter(|b| b.must_have).count();
    assert!(must_have >= 140, "{must_have}");
    let issuers = registry
        .brands()
        .iter()
        .filter(|b| b.holder.is_some())
        .count();
    assert_eq!(issuers, 16);
    for brand in super::registry::tests::fixture_registry().brands() {
        for name in &brand.names {
            assert!(
                registry.exact(name).is_some_and(|b| b.name == brand.name),
                "{name} ({}) must be the migration's name of the same brand",
                brand.name
            );
        }
    }
}

/// The holder domains of `mazimi_core::holder` are exactly what the
/// database accepts (20261111000102).
#[tokio::test]
async fn holder_domains_match_the_database_checks() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    use mazimi_core::holder::HolderField;
    let user: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("holder-{}@test.example", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_profiles (user_id, region_code, updated_at) VALUES ($1, 'busan', now())",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    for field in HolderField::ALL {
        for code in field.domain() {
            let sql = if field.is_set() {
                format!(
                    "UPDATE user_profiles SET {} = ARRAY[$2]::text[] WHERE user_id = $1",
                    field.name()
                )
            } else {
                format!(
                    "UPDATE user_profiles SET {} = $2 WHERE user_id = $1",
                    field.name()
                )
            };
            sqlx::query(&sql)
                .bind(user)
                .bind(*code)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("{} {code}: {e}", field.name()));
        }
        let sql = if field.is_set() {
            format!(
                "UPDATE user_profiles SET {} = ARRAY['visa']::text[] WHERE user_id = $1",
                field.name()
            )
        } else {
            format!(
                "UPDATE user_profiles SET {} = 'visa' WHERE user_id = $1",
                field.name()
            )
        };
        assert!(
            sqlx::query(&sql).bind(user).execute(&pool).await.is_err(),
            "{}",
            field.name()
        );
    }
    // Every issuer's key is a code of its field.
    let keys: Vec<(String, String)> = sqlx::query_as(
        "SELECT holder_field, holder_value FROM brands WHERE holder_field IS NOT NULL",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for (field, value) in keys {
        let field = HolderField::parse(&field).expect("known field");
        assert!(field.domain().contains(&value.as_str()), "{value}");
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

/// Slice 0: the Naver residue migration clears titles and queries, rejects
/// what our bot never fetched, and keeps what it did.
#[tokio::test]
async fn naver_residue_is_cleared() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (_, url) = door(&pool, "gongcha", "page", None, "/x").await;
    let domain: Uuid =
        sqlx::query_scalar("SELECT domain_id FROM corporate_doors WHERE entry_url = $1")
            .bind(&url)
            .fetch_one(&pool)
            .await
            .unwrap();
    let never: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, discovery_query,
                                                page_title, status)
         VALUES ($1, $2, $2, 'search_api', '공차 할인', '네이버 검색 제목', 'queued') RETURNING id",
    )
    .bind(domain)
    .bind(format!("{url}/never"))
    .fetch_one(&pool)
    .await
    .unwrap();
    let fetched: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, discovery_query,
                                                page_title, status, last_checked_at, content_hash)
         VALUES ($1, $2, $2, 'search_api', '공차', '제목', 'review', now(), repeat('a', 32)) RETURNING id",
    )
    .bind(domain)
    .bind(format!("{url}/fetched"))
    .fetch_one(&pool)
    .await
    .unwrap();
    let approved: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, page_title, status)
         VALUES ($1, $2, $2, 'search_api', '제목', 'approved') RETURNING id",
    )
    .bind(domain)
    .bind(format!("{url}/approved"))
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../../infra/migrations/20261111000101_naver_search_residue.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let rows: Vec<(Uuid, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT id, page_title, discovery_query, status FROM benefit_source_candidates WHERE id = ANY($1)",
    )
    .bind(vec![never, fetched, approved])
    .fetch_all(&pool)
    .await
    .unwrap();
    for (id, title, query, status) in rows {
        assert_eq!((title, query), (None, None));
        let expected = if id == never {
            "rejected"
        } else if id == approved {
            "approved"
        } else {
            "review"
        };
        assert_eq!(status, expected, "a person's approval is never undone");
    }
    let residue: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM benefit_source_candidates WHERE discovered_via = 'search_api' AND page_title IS NOT NULL)
              + (SELECT count(*) FROM brand_source_suggestions WHERE discovered_via IN ('naver_search', 'search_api') AND page_title IS NOT NULL)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(residue, 0);
}

/// Door reads are kept 180 days by the storage job's explicit allowlist.
#[tokio::test]
async fn door_reads_are_pruned_after_their_window_only() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, _) = door(&pool, "gongcha", "page", None, "/r").await;
    let old: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_door_reads (door_id, mode, verdict, read_at) VALUES ($1, 'shadow', 'proposal', now() - interval '200 days') RETURNING id",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let recent: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_door_reads (door_id, mode, verdict, read_at) VALUES ($1, 'shadow', 'proposal', now() - interval '100 days') RETURNING id",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    crate::storage::prune(
        &pool,
        "corporate_door_reads",
        crate::storage::DOOR_READS_RETENTION_DAYS,
        1_000,
    )
    .await
    .unwrap();
    let left: BTreeSet<Uuid> =
        sqlx::query_scalar("SELECT id FROM corporate_door_reads WHERE door_id = $1")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .collect();
    assert!(!left.contains(&old) && left.contains(&recent));
}

/// Not a check: fills the shadow ledger from the saved pages (two reads a
/// day apart of each hub and of 공차's page) so `corporate-doors metrics`
/// can be shown on fixture data. Run with `--ignored`; leaves its doors.
#[tokio::test]
#[ignore]
async fn shadow_metrics_on_fixtures() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    clean(&pool).await;
    let page =
        std::fs::read_to_string(format!("{PAGES}/gong-cha.co.kr_skt_membership.html")).unwrap();
    let mut doors = Vec::new();
    let mut replies: Vec<(String, crate::corporate_auto::http::HttpReply)> = Vec::new();
    for (grantor, kind, path, body) in [
        (
            "skt",
            "hub_rows",
            "/list",
            hub("sktmembership_list-tab1.html"),
        ),
        (
            "kbcard",
            "hub_rows",
            "/brand",
            hub("kbcard_brand_discounts.html"),
        ),
        (
            "payco",
            "hub_rows",
            "/coupon-list",
            hub("payco_coupon_list.html"),
        ),
        (
            "gongcha",
            "page",
            "/brand/store/skt_membership",
            page.clone(),
        ),
    ] {
        let (id, url) = door(&pool, grantor, kind, Some("explicit_permission"), path).await;
        replies.push((robots_url(&url), reply(200, "text/plain", ALLOW_ALL)));
        replies.push((url, reply(200, "text/html", &body)));
        doors.push(id);
    }
    let http = ScriptedHttp::new(
        replies
            .iter()
            .map(|(u, r)| (u.as_str(), r.clone()))
            .collect(),
    );
    let shadow = flags(DoorsMode::Shadow, &[]);
    run_with(&pool, &shadow, &http, spacing(), Some(&doors))
        .await
        .unwrap();
    for id in &doors {
        due_now(&pool, *id).await;
    }
    run_with(&pool, &shadow, &http, spacing(), Some(&doors))
        .await
        .unwrap();
    let m = super::metrics::collect(&pool, 14).await.unwrap();
    eprintln!("{}", super::metrics::render(&m));
}

/// Publishes the T 멤버십 fixture's offers through a live hub door and
/// returns (door, url, programmes shown).
async fn published_hub(pool: &PgPool) -> (Uuid, String, Vec<Uuid>, ScriptedHttp) {
    clean(pool).await;
    let (id, url) = door(
        pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/list",
    )
    .await;
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (
            url.as_str(),
            reply(200, "text/html", &hub("sktmembership_list-tab1.html")),
        ),
    ]);
    let live = flags(DoorsMode::Live, &[Kind::HubRows]);
    run_with(pool, &live, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    age_offers(pool, id).await;
    publish_all(pool, &live, &http, id).await;
    let shown: Vec<Uuid> = sqlx::query_scalar(
        "SELECT o.program_id FROM corporate_door_offers o JOIN current_programs p ON p.id = o.program_id
         WHERE o.door_id = $1",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(shown.len() >= 5, "{}", shown.len());
    (id, url, shown, http)
}

async fn still_shown(pool: &PgPool, programmes: &[Uuid]) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM current_programs WHERE id = ANY($1)")
        .bind(programmes)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Ages the door monitors: no live confirming read for `days` days.
async fn no_live_read_for(pool: &PgPool, programmes: &[Uuid], days: i32) {
    sqlx::query(
        "UPDATE program_source_monitors SET review_due_at = now() + interval '3 days' - make_interval(days => $2),
                                            verified_at = now() - make_interval(days => $2)
         WHERE program_id = ANY($1) AND verification_mode = 'door'",
    )
    .bind(programmes)
    .bind(days)
    .execute(pool)
    .await
    .unwrap();
}

/// Code review C-1, security review M4, verify F4: whatever turns the hub
/// lane off — the mode off, the kind removed, intake not live — the shown
/// offers leave within three days of the last live read, by the sweep that
/// runs whatever the switches say; and shadow reads never extend them.
#[tokio::test]
async fn turning_the_hub_lane_off_hides_what_it_showed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let today = crate::corporate_auto::health::kst_today();
    for case in ["mode_off", "kind_removed", "intake_not_live"] {
        let (id, _, shown, http) = published_hub(&pool).await;
        // Reads go on in shadow (kind removed / intake not live), or not at
        // all (mode off): either way no live confirmation.
        let after = match case {
            "mode_off" => flags(DoorsMode::Off, &[]),
            "kind_removed" => flags(DoorsMode::Live, &[Kind::Page]),
            _ => DoorFlags {
                intake_live: false,
                ..flags(DoorsMode::Live, &[Kind::HubRows])
            },
        };
        assert!(!after.publishes(Kind::HubRows), "{case}");
        due_now(&pool, id).await;
        run_with(&pool, &after, &http, spacing(), Some(&[id]))
            .await
            .unwrap();
        lifecycle::sweep(&pool, today).await.unwrap();
        assert_eq!(
            still_shown(&pool, &shown).await,
            shown.len() as i64,
            "{case}: within 3 days still shown"
        );
        no_live_read_for(&pool, &shown, 4).await;
        due_now(&pool, id).await;
        run_with(&pool, &after, &http, spacing(), Some(&[id]))
            .await
            .unwrap();
        lifecycle::sweep(&pool, today).await.unwrap();
        assert_eq!(
            still_shown(&pool, &shown).await,
            0,
            "{case}: hidden after 3 days without a live read"
        );
    }
}

/// Verify F4's ten-day scenario through the existing fail-closed sweep
/// alone (no door job at all): the door candidate is `approved`, so
/// `hold_overdue_sources` holds every overdue hub programme.
#[tokio::test]
async fn the_fail_closed_overdue_sweep_holds_hub_offers_after_ten_days() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (_, _, shown, _) = published_hub(&pool).await;
    no_live_read_for(&pool, &shown, 10).await;
    crate::source_monitoring::hold_overdue_sources(&pool, chrono::Duration::hours(72), 1_000)
        .await
        .unwrap();
    assert_eq!(still_shown(&pool, &shown).await, 0);
}

/// Code review B2: a hub door excludes its own list page, never its
/// domain: another monitored page on the hub host stays with every reader.
#[tokio::test]
async fn a_hub_door_excludes_its_candidate_not_its_domain() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, _, shown, _) = published_hub(&pool).await;
    let (domain, hub_candidate): (Uuid, Uuid) =
        sqlx::query_as("SELECT domain_id, candidate_id FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    // Another programme of the same host, monitored as every other source.
    let host: String =
        sqlx::query_scalar("SELECT domain FROM benefit_source_domains WHERE id = $1")
            .bind(domain)
            .fetch_one(&pool)
            .await
            .unwrap();
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, status)
         VALUES ($1, $2, $2, 'manual', 'approved') RETURNING id",
    )
    .bind(domain)
    .bind(format!("https://{host}/other-event"))
    .fetch_one(&pool)
    .await
    .unwrap();
    let programme: Uuid = sqlx::query_scalar(
        "INSERT INTO programs (program_type, source_type, source_id, title, program_status, is_active)
         VALUES ('corporate_benefit', 'corporate_official', $1, 'KB 다른 혜택', 'review_required', false) RETURNING id",
    )
    .bind(format!("b2:{other}"))
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at,
                                              review_due_at, verification_mode)
         VALUES ($1, $2, 'current', now(), now() + interval '30 days', 'evidence_auto')",
    )
    .bind(programme)
    .bind(other)
    .execute(&pool)
    .await
    .unwrap();
    let eligible: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM benefit_source_domains d WHERE d.id = $1 AND {})",
        crate::corporate_auto::policy::ELIGIBLE_DOMAIN_SQL
    ))
    .bind(domain)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(eligible, "the hub host stays eligible for its other pages");
    let owned = |candidate: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(&format!(
                "SELECT {} FROM benefit_source_candidates c WHERE c.id = $1",
                crate::corporate_auto::policy::hub_owned("c")
            ))
            .bind(candidate)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert!(owned(hub_candidate).await);
    assert!(!owned(other).await);
    assert!(!shown.is_empty());
}

/// Security review H4: with the hub lane off and its monitors overdue, on a
/// crawl-approved hub domain, the hash fetcher never claims the hub's list
/// page (it still claims another due page of the host).
#[tokio::test]
async fn no_other_job_reads_a_hub_list_page() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, _, shown, _) = published_hub(&pool).await;
    let (domain, hub_candidate): (Uuid, Uuid) =
        sqlx::query_as("SELECT domain_id, candidate_id FROM corporate_doors WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE benefit_source_domains SET crawl_allowed = true, compliance_status = 'approved',
                approval_basis = 'terms_explicit', approval_reference = 'test', terms_checked_at = now(),
                terms_review_outcome = 'explicit_permission', robots_status = 'allowed', robots_checked_at = now(),
                robots_content_hash = repeat('a', 32), allowed_path_prefixes = ARRAY['/'],
                robots_reviewed_path_prefixes = ARRAY['/'], crawler_contact_url = 'https://mazimi.example/bot',
                compliance_reviewed_by = 'test', compliance_reviewed_at = now(),
                approval_expires_at = now() + interval '30 days', collection_mode = 'direct_public'
         WHERE id = $1",
    )
    .bind(domain)
    .execute(&pool)
    .await
    .unwrap();
    no_live_read_for(&pool, &shown, 10).await;
    crate::source_monitoring::hold_overdue_sources(&pool, chrono::Duration::hours(72), 1_000)
        .await
        .unwrap();
    sqlx::query("UPDATE benefit_source_candidates SET next_check_at = now() - interval '1 minute' WHERE id = $1")
        .bind(hub_candidate)
        .execute(&pool)
        .await
        .unwrap();
    let claimed = crate::sources::benefit_pages::BenefitPageFetcher::for_tests()
        .claim_ids_for_tests(&pool)
        .await
        .unwrap();
    assert!(
        !claimed.contains(&hub_candidate),
        "the hub's list page is the door job's alone"
    );
}

/// Security review H5: a redeem brand with no source domain of ours opts
/// out on its own registry domain, and its hub offer is hidden at once
/// (the sweep runs whatever the switches say).
#[tokio::test]
async fn a_redeem_brand_without_a_source_domain_can_opt_out() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, _, _, _) = published_hub(&pool).await;
    let gongcha = brand(&pool, "gongcha").await;
    let host = format!("gongcha-{}.example", Uuid::new_v4().simple());
    sqlx::query("UPDATE brand_discovery_targets SET official_domain = $2 WHERE brand_id = $1")
        .bind(gongcha)
        .bind(&host)
        .execute(&pool)
        .await
        .unwrap();
    let program: Uuid = sqlx::query_scalar(
        "SELECT program_id FROM corporate_door_offers WHERE door_id = $1 AND redeem_brand_id = $2 AND status = 'published'",
    )
    .bind(id)
    .bind(gongcha)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO benefit_source_optouts (domain, scope, channel, effective_at, domain_ids)
         VALUES ($1, 'display', 'web_form', now(), ARRAY[]::uuid[])",
    )
    .bind(&host)
    .execute(&pool)
    .await
    .unwrap();
    lifecycle::sweep(&pool, crate::corporate_auto::health::kst_today())
        .await
        .unwrap();
    assert_eq!(still_shown(&pool, &[program]).await, 0);
    sqlx::query("DELETE FROM benefit_source_optouts WHERE domain = $1")
        .bind(&host)
        .execute(&pool)
        .await
        .unwrap();
}

/// Code review B3: with two active `SK텔레콤` brand rows (the SKT 청년 seeds
/// sponsor whichever ranks first), the registry migration gives every row
/// of that name the holder key, so the seeds' sponsor carries it, and the
/// registry still has one SK텔레콤.
#[tokio::test]
async fn the_holder_key_reaches_the_seeds_sponsor_when_brand_names_repeat() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let duplicate: Uuid = sqlx::query_scalar(
        "INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
         VALUES ('SK텔레콤', 'sk텔레콤', 'b3_test', $1, 100) RETURNING id",
    )
    .bind(Uuid::new_v4().to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../../infra/migrations/20261111000103_brand_registry_must_have.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let key: Option<String> = sqlx::query_scalar("SELECT holder_value FROM brands WHERE id = $1")
        .bind(duplicate)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(key.as_deref(), Some("skt"));
    let unkeyed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_brands pb JOIN programs p ON p.id = pb.program_id
         JOIN brands b ON b.id = pb.brand_id
         WHERE p.source_id LIKE 'issuer:20261106:skt:%' AND pb.relationship_type = 'sponsor'
           AND b.holder_field IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unkeyed, 0);
    let registry = Registry::load(&pool).await.unwrap();
    assert_eq!(registry.shared_names(), Vec::new());
    assert!(registry.exact("SK텔레콤").is_some());
    sqlx::query("DELETE FROM brands WHERE id = $1")
        .bind(duplicate)
        .execute(&pool)
        .await
        .unwrap();
}

/// Review H-4: a list adds its dated article rows only (never its footer),
/// a child whose verdict is final is retired, and the cap counts active
/// children, so retired ones never block the next article.
#[tokio::test]
async fn list_children_are_articles_retire_when_final_and_leave_room() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (list, url) = door(&pool, "gongcha", "listing", None, "/news").await;
    let host = reqwest::Url::parse(&url)
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let page = |from: u32| {
        let articles: String = (from..from + 6)
            .map(|i| format!("<li><a href=\"/news/{i}\">공차 11월 멤버십 할인 이벤트 안내 {i}</a><span>2026.11.0{}</span></li>", i % 9 + 1))
            .collect();
        let footer: String = (0..8)
            .map(|i| format!("<li><a href=\"/footer/p{i}\">약관 {i}</a></li>"))
            .collect();
        format!(
            "<html><body><ul>{articles}</ul><footer><ul>{footer}</ul></footer>{}</body></html>",
            "<p>공차 매장 소식과 새로운 메뉴 안내</p>".repeat(120)
        )
    };
    let shadow = flags(DoorsMode::Shadow, &[]);
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (url.as_str(), reply(200, "text/html", &page(1))),
    ]);
    run_with(&pool, &shadow, &http, spacing(), Some(&[list]))
        .await
        .unwrap();
    let children: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, entry_url FROM corporate_doors WHERE parent_door_id = $1 ORDER BY entry_url",
    )
    .bind(list)
    .fetch_all(&pool)
    .await
    .unwrap();
    let paths: BTreeSet<String> = children
        .iter()
        .map(|(_, u)| reqwest::Url::parse(u).unwrap().path().to_string())
        .collect();
    assert_eq!(
        paths,
        (1..=6)
            .map(|i| format!("/news/{i}"))
            .collect::<BTreeSet<_>>()
    );

    // An article published long ago: retired at once with its verdict.
    let old = format!(
        "<html><head><meta property=\"article:published_time\" content=\"2025-01-02\"></head><body><h1>공차 할인</h1><p>T 멤버십 회원 전 메뉴 10% 할인 (~2025.01.31)</p>{}</body></html>",
        "<p>공차 매장 소식과 새로운 메뉴 안내</p>".repeat(120)
    );
    let undated = format!(
        "<html><body><h1>공차 할인</h1><p>전 메뉴 10% 할인</p>{}</body></html>",
        "<p>공차 매장 소식과 새로운 메뉴 안내</p>".repeat(120)
    );
    let (first, first_url) = &children[0];
    let (second, second_url) = &children[1];
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (first_url.as_str(), reply(200, "text/html", &old)),
        (second_url.as_str(), reply(200, "text/html", &undated)),
    ]);
    let state = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT state, last_outcome FROM corporate_doors WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    for read in 1..=3 {
        due_now(&pool, *first).await;
        due_now(&pool, *second).await;
        run_with(&pool, &shadow, &http, spacing(), Some(&[*first, *second]))
            .await
            .unwrap();
        assert_eq!(
            state(*first).await,
            ("retired".into(), Some("stale_page".into()))
        );
        let (second_state, outcome) = state(*second).await;
        assert_eq!(outcome.as_deref(), Some("article_undated"), "read {read}");
        assert_eq!(second_state == "retired", read == 3, "read {read}");
    }
    // A retired child is never read again.
    let before = http.requested().len();
    due_now(&pool, *first).await;
    run_with(&pool, &shadow, &http, spacing(), Some(&[*first]))
        .await
        .unwrap();
    assert_eq!(http.requested().len(), before);

    // 60 retired children do not fill the cap: the next articles are added.
    for i in 0..job::MAX_CHILDREN {
        sqlx::query(
            "INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind, parent_door_id, state)
             SELECT domain_id, $2, grantor_brand_id, 'page', id, 'retired' FROM corporate_doors WHERE id = $1",
        )
        .bind(list)
        .bind(format!("https://{host}/old/{i}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let http = ScriptedHttp::new(vec![
        (
            robots_url(&url).as_str(),
            reply(200, "text/plain", ALLOW_ALL),
        ),
        (url.as_str(), reply(200, "text/html", &page(7))),
    ]);
    due_now(&pool, list).await;
    run_with(&pool, &shadow, &http, spacing(), Some(&[list]))
        .await
        .unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_doors WHERE parent_door_id = $1 AND state <> 'retired'",
    )
    .bind(list)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        active,
        4 + 6,
        "four earlier articles still active, six new ones"
    );
}

/// Verify finding F9: every table in `public` (these three and every later
/// one) has row level security enabled and forced. The one exception is
/// the migration bookkeeping table the migrator owns.
#[tokio::test]
async fn every_public_table_forces_row_level_security() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    const EXCEPTIONS: &[&str] = &["_sqlx_migrations"];
    let open: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
           AND NOT (c.relrowsecurity AND c.relforcerowsecurity)
           AND NOT (c.relname = ANY($1))
         ORDER BY 1",
    )
    .bind(EXCEPTIONS)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(open, Vec::<String>::new());
    let doors: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class WHERE relname IN
             ('corporate_doors', 'corporate_door_reads', 'corporate_door_offers', 'corporate_benefit_identities')
           AND relrowsecurity AND relforcerowsecurity",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(doors, 4);
}

/// Verify finding F8, security review L2: a pass probes at most three
/// same-host sitemap URLs (never another host, never a door's URL), none
/// on a second pass the same day, the next three the day after, and a
/// door lost to robots is not looked for at all.
#[tokio::test]
async fn rediscovery_probes_three_new_same_host_urls_a_day() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (id, url) = door(
        &pool,
        "skt",
        "hub_rows",
        Some("explicit_permission"),
        "/old/list",
    )
    .await;
    let host = reqwest::Url::parse(&url)
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let skt = brand(&pool, "skt").await;
    sqlx::query(
        "UPDATE corporate_doors SET state = 'lost', lost_since = now(), last_matched_brands = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(vec![skt])
    .execute(&pool)
    .await
    .unwrap();
    // Another door on the host: its URL is never probed.
    let taken = format!("https://{host}/old/taken");
    sqlx::query(
        "INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind)
         SELECT domain_id, $2, grantor_brand_id, 'page' FROM corporate_doors WHERE id = $1",
    )
    .bind(id)
    .bind(&taken)
    .execute(&pool)
    .await
    .unwrap();
    let same: Vec<String> = (1..=8)
        .map(|i| format!("https://{host}/old/p{i}"))
        .collect();
    let off = "https://elsewhere.example/old/list".to_string();
    let locs: String = same
        .iter()
        .chain([&off, &taken])
        .map(|u| format!("<url><loc>{u}</loc></url>"))
        .collect();
    let sitemap = format!("<?xml version=\"1.0\"?><urlset>{locs}</urlset>");
    let robots = format!("User-agent: *\nAllow: /\nSitemap: https://{host}/sm.xml\n");
    let kb = hub("kbcard_brand_discounts.html");
    let mut replies = vec![
        (
            format!("https://{host}/robots.txt"),
            reply(200, "text/plain", &robots),
        ),
        (
            format!("https://{host}/sm.xml"),
            reply(200, "text/plain", &sitemap),
        ),
        (
            format!("https://{host}/sitemap.xml"),
            reply(404, "text/plain", ""),
        ),
    ];
    for u in &same {
        replies.push((u.clone(), reply(200, "text/html", &kb)));
    }
    let http = ScriptedHttp::new(
        replies
            .iter()
            .map(|(u, r)| (u.as_str(), r.clone()))
            .collect(),
    );
    let shadow = flags(DoorsMode::Shadow, &[]);
    let pages = |http: &ScriptedHttp| -> Vec<String> {
        http.requested()
            .into_iter()
            .filter(|u| !u.ends_with(".xml") && !u.ends_with("robots.txt"))
            .collect()
    };
    assert_eq!(
        lifecycle::rediscover(&pool, &shadow, &http, spacing(), Some(&[id]))
            .await
            .unwrap(),
        0
    );
    let first = pages(&http);
    assert_eq!(first.len(), 3, "{first:?}");
    assert!(first.iter().all(|u| same.contains(u)), "{first:?}");
    // The same day: nothing.
    lifecycle::rediscover(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(pages(&http).len(), 3);
    // The next day: three other URLs.
    sqlx::query("UPDATE corporate_door_reads SET read_at = read_at - interval '25 hours' WHERE door_id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    lifecycle::rediscover(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    let both = pages(&http);
    assert_eq!(both.len(), 6, "{both:?}");
    assert_eq!(
        both.iter().collect::<BTreeSet<_>>().len(),
        6,
        "no URL twice"
    );
    assert!(!both.contains(&off) && !both.contains(&taken));
    // Lost to robots: not looked for.
    sqlx::query("UPDATE corporate_doors SET last_outcome = 'robots_disallow' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE corporate_door_reads SET read_at = read_at - interval '25 hours' WHERE door_id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let before = http.requested().len();
    lifecycle::rediscover(&pool, &shadow, &http, spacing(), Some(&[id]))
        .await
        .unwrap();
    assert_eq!(http.requested().len(), before);
}

/// Code review M-15: a hub programme hidden 30 days is retired (kept, never
/// deleted); before that it is only hidden.
#[tokio::test]
async fn a_hub_programme_hidden_thirty_days_is_retired_not_deleted() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let (_, _, shown, _) = published_hub(&pool).await;
    no_live_read_for(&pool, &shown, 10).await;
    let today = crate::corporate_auto::health::kst_today();
    lifecycle::sweep(&pool, today).await.unwrap();
    assert_eq!(still_shown(&pool, &shown).await, 0);
    let retired = |pool: PgPool, shown: Vec<Uuid>| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM programs WHERE id = ANY($1) AND retirement_reason = 'provider_withdrawn'",
        )
        .bind(&shown)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        retired(pool.clone(), shown.clone()).await,
        0,
        "hidden, not yet retired"
    );
    sqlx::query("UPDATE programs SET updated_at = now() - interval '31 days' WHERE id = ANY($1)")
        .bind(&shown)
        .execute(&pool)
        .await
        .unwrap();
    lifecycle::sweep(&pool, today).await.unwrap();
    assert_eq!(
        retired(pool.clone(), shown.clone()).await,
        shown.len() as i64
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM programs WHERE id = ANY($1)")
        .bind(&shown)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, shown.len() as i64, "never deleted");
}
