//! Re-review of 31536232 (reviewer checkout only, never committed). Mounted
//! under `resilience::tests`. Part N: active offers against the two-signal
//! rule and the URL/mainEntity structured rule. Part DB: deadline
//! extension, sweep and selector on a throwaway Postgres.

use super::*;
use crate::corporate_auto::evidence::visible_text;

const NQ: &str = "학생 인증 시 10% 할인";
const NURL: &str = "https://b.example/student";

fn n_page(main: &str, head: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>학생 혜택 | 브랜드</title>{head}</head>\
         <body><header class=\"hd\"><h1 class=\"logo\"><a href=\"/\">브랜드</a></h1></header>\
         <main id=\"content\">{main}</main><footer class=\"ft\"><p>고객센터 1588-1234</p></footer></body></html>"
    )
}

fn n_resolve(html: &str, quote: &str) -> Resolution {
    let now = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap().and_hms_opt(12, 0, 0).unwrap();
    let view = PageView::at(html, &visible_text(html), now).with_url(NURL);
    resolve(
        &view,
        &StoredFact {
            quote,
            baseline: None,
            ages: Ages { min: None, max: None },
            adoption_open: true,
            anchors: &Anchors::default(),
        },
    )
}

fn n_key(r: &Resolution) -> String {
    match r {
        Resolution::Ambiguous(reason) => reason_key(reason).unwrap_or("other").to_string(),
        other => other.label().to_string(),
    }
}

#[test]
fn r9b_part_n_two_signal_and_structured() {
    let q = NQ;
    let ld = |j: &str| format!("<script type=\"application/ld+json\">{j}</script>");
    let sec = |inner: &str| format!("<section class=\"evt\"><h2>학생 할인</h2><p>{q}</p>{inner}</section>");
    let cases: Vec<(&str, &str, String)> = vec![
        ("N1", "extended event: period passed + 본 이벤트는 종료 + 연장 진행", n_page(&sec("<p>이벤트 기간: 2026.09.01 ~ 2026.09.30</p><p>본 이벤트는 종료되었습니다. 성원에 힘입어 2026년 11월 30일까지 연장 이벤트를 진행합니다.</p>"), "")),
        ("N2", "phase 1 closed, phase 2 described in prose", n_page(&sec("<p>이벤트 기간: 2026.09.01 ~ 2026.09.30</p><p>본 이벤트는 마감되었습니다. 2차 학생 할인은 10월 15일부터 11월 30일까지 진행됩니다.</p>"), "")),
        ("N3", "evergreen offer + leftover sub-event box (div.evt-box)", n_page(&sec("<div class=\"evt-box\"><p>신학기 추가 쿠폰</p><p>이벤트 기간: 2026.03.01 ~ 2026.03.31</p><p>본 이벤트는 종료되었습니다.</p></div>"), "")),
        ("N4", "same, sub-event in a banner div", n_page(&sec("<div class=\"event-banner\"><strong>여름방학 특별전</strong><p>행사 기간: 2026.07.01 ~ 2026.08.31</p><p>해당 행사는 종료되었습니다.</p></div>"), "")),
        ("N5", "table: own row passed + 비고 says the event ended, 상시 학생 할인", n_page(&sec("<table><tbody><tr><th>이벤트 기간</th><td>2026.09.01 ~ 2026.09.30</td></tr><tr><th>비고</th><td>해당 이벤트는 종료되었습니다. 학생 할인은 상시 적용됩니다.</td></tr></tbody></table>"), "")),
        ("N6", "dl: launch period passed + 이번 프로모션은 종료, discount continues", n_page(&sec("<dl><dt>프로모션 기간</dt><dd>2026.09.01 ~ 2026.09.30</dd></dl><p>이번 프로모션은 종료되었으며, 학생 인증 할인은 계속 제공됩니다.</p>"), "")),
        ("N7", "English: Offer period passed + This promotion has ended, student pricing continues", n_page(&format!("<section><h2>Students</h2><p>{q}</p><p>Offer period: 2026.09.01 ~ 2026.09.30</p><p>This promotion has ended. Student pricing continues year-round.</p></section>"), "")),
        ("N8", "mainEntity: a past launch Event on an evergreen page (structured alone)", n_page(&sec(""), &ld("{\"@context\":\"https://schema.org\",\"@type\":\"WebPage\",\"mainEntity\":{\"@type\":\"Event\",\"name\":\"신학기 런칭 이벤트\",\"startDate\":\"2026-03-01\",\"endDate\":\"2026-03-31\"}}"))),
        ("N9", "Offer at this URL with a stale SEO validThrough", n_page(&sec(""), &ld(&format!("{{\"@context\":\"https://schema.org\",\"@type\":\"Offer\",\"url\":\"{NURL}\",\"price\":\"0\",\"validThrough\":\"2025-12-31\"}}")))),
        ("N10", "Product at this URL whose nested offer is sold out for one size", n_page(&sec(""), &ld(&format!("{{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"url\":\"{NURL}\",\"offers\":[{{\"@type\":\"Offer\",\"url\":\"{NURL}\",\"availability\":\"https://schema.org/SoldOut\",\"name\":\"XS\"}},{{\"@type\":\"Offer\",\"availability\":\"https://schema.org/InStock\"}}]}}")))),
        ("N11", "mainEntity FAQPage-style Event list item cancelled", n_page(&sec(""), &ld("{\"@context\":\"https://schema.org\",\"@type\":\"WebPage\",\"mainEntity\":[{\"@type\":\"Event\",\"name\":\"오프라인 설명회\",\"eventStatus\":\"https://schema.org/EventCancelled\"}]}"))),
        ("N12", "control-ended: period passed + 본 이벤트는 종료되었습니다", n_page(&sec("<p>이벤트 기간: 2026.09.01 ~ 2026.09.30</p><p>본 이벤트는 종료되었습니다.</p>"), "")),
        ("C1", "r9 C1: 이벤트 기간 passed only", n_page(&sec("<p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>"), "")),
        ("C2", "r9 C2: 본 이벤트는 종료되었습니다 only", n_page(&sec("<p>본 이벤트는 종료되었습니다.</p>"), "")),
    ];
    for (id, name, html) in &cases {
        let r = n_resolve(html, q);
        let key = n_key(&r);
        let trusted = !r.confirms() && crate::corporate_auto::DEFAULT_HIDE_REASONS.contains(&key.as_str());
        println!("R9B {id} {} key={key} hide_only_hides={trusted} | {name}", if r.confirms() { "shown" } else { "HIDDEN_live" });
    }
}

mod r9b_db {
    use crate::corporate_auto::evidence::{compact, read_quote, run_with, split_fragments};
    use crate::corporate_auto::http::LoopbackHttp;
    use crate::corporate_auto::VerifyMode;
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";
    const QUOTE: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    type Page = Arc<Mutex<(u16, String)>>;

    fn good() -> String {
        "<html><head><title>t</title></head><body><main><h2>학생 할인</h2>\
         <p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><p>대상 상품 최대 12개월</p></main></body></html>".into()
    }
    /// The offer ended: the page says so and its period passed, but in the
    /// forms the default allowlist no longer trusts on their own.
    fn ended_one_signal() -> String {
        "<html><head><title>t</title></head><body><main><h2>학생 할인</h2>\
         <p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><p>본 이벤트는 종료되었습니다.</p></main></body></html>".into()
    }
    /// The offer was taken off the page.
    fn removed() -> String {
        "<html><head><title>t</title></head><body><main><h2>멤버십 안내</h2>\
         <p>학생 할인 프로그램은 운영을 마쳤습니다. 다른 혜택을 확인해 주세요.</p></main></body></html>".into()
    }

    async fn db() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap())
    }

    fn env(mode: &str) {
        std::env::set_var("CORPORATE_AUTO_VERIFY_ENABLED", "1");
        std::env::set_var("CORPORATE_AUTO_VERIFY_MODE", mode);
        std::env::set_var("CORPORATE_BOT_CONTACT_URL", "https://mazimi.example/crawler-policy");
        std::env::remove_var("CORPORATE_AUTO_HIDE_REASONS");
    }
    fn clear() {
        for k in ["CORPORATE_AUTO_VERIFY_ENABLED", "CORPORATE_AUTO_VERIFY_MODE", "CORPORATE_BOT_CONTACT_URL", "CORPORATE_AUTO_HIDE_REASONS"] {
            std::env::remove_var(k);
        }
    }

    async fn serve(page: Page) -> String {
        use axum::{response::IntoResponse, routing::get, Router};
        let p = page.clone();
        let app = Router::new()
            .route("/robots.txt", get(|| async { (axum::http::StatusCode::OK, [("content-type", "text/plain")], "User-agent: *\nAllow: /\n").into_response() }))
            .route("/benefit", get(move || {
                let p = p.clone();
                async move {
                    let (s, b) = p.lock().unwrap().clone();
                    (axum::http::StatusCode::from_u16(s).unwrap(), [("content-type", "text/html; charset=utf-8")], b).into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/benefit")
    }

    struct Fx { candidate_id: Uuid, program_id: Uuid, domain_id: Uuid }

    /// Published, monitor current, deadline 12 h away; the domain is not
    /// crawl-approved, so the hash fetcher never claims it (the exposed 12).
    async fn published(pool: &PgPool, url: &str) -> Fx {
        let (domain_id, candidate_id, program_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
                     VALUES($1,'r9b',$2,'brand_official','verified','fixture',now(),'pending','manual_review')")
            .bind(domain_id).bind(format!("{}.evidence.example", domain_id.simple())).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at)
                     VALUES($1,$2,$3,$3 || '#' || $1::text,'manual','approved','detail',now())")
            .bind(candidate_id).bind(domain_id).bind(url).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                     VALUES($1,'corporate_benefit','corporate_official',$2,'r9b 학생 할인','학생 10% 할인','픽스처',$3,'always_open','corporate','discount',true,'current',ARRAY[]::text[])")
            .bind(program_id).bind(format!("r9b:{program_id}")).bind(url).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode,verified_content_hash,recheck_interval_seconds)
                     VALUES($1,$2,'current',now()-interval '14 days',now()+interval '12 hours','evidence_auto',$3,86400)")
            .bind(program_id).bind(candidate_id).bind("d".repeat(32)).execute(pool).await.unwrap();
        for fragment in split_fragments(QUOTE) {
            let (fact_type, value) = read_quote(&fragment);
            let key: String = Sha256::digest(format!("{program_id}:{}", compact(&fragment)).as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,adopt_before)
                         VALUES($1,$2,$3,$4,$5,'fixture',now()+interval '1 day')")
                .bind(program_id).bind(fact_type).bind(value).bind(&fragment).bind(key).execute(pool).await.unwrap();
        }
        let run_id: Uuid = sqlx::query_scalar("INSERT INTO corporate_census_runs(status,finished_at) VALUES('completed',now()) RETURNING id").fetch_one(pool).await.unwrap();
        sqlx::query("INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
                     VALUES($1,'monitor',$2,$3,$4,$5,'direct','direct_html',true,'ok','none')")
            .bind(run_id).bind(domain_id).bind(candidate_id).bind(program_id).bind(url).execute(pool).await.unwrap();
        Fx { candidate_id, program_id, domain_id }
    }

    async fn cleanup(pool: &PgPool, f: &Fx) {
        sqlx::query("DELETE FROM operational_events WHERE fingerprint LIKE '%' || $1::text").bind(f.candidate_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1").bind(f.program_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1").bind(f.candidate_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM corporate_census_runs WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE domain_id=$1)").bind(f.domain_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1").bind(f.domain_id).execute(pool).await.unwrap();
    }

    /// One scheduled run (no forced next_check_at): what the selector picks.
    async fn scheduled(pool: &PgPool, f: &Fx, mode: VerifyMode) -> usize {
        let http = LoopbackHttp::new(UA);
        run_with(pool, mode, &http, Some(vec![f.candidate_id]), Some(std::time::Duration::from_millis(20))).await.unwrap().targets
    }

    async fn state(pool: &PgPool, f: &Fx) -> (bool, f64, i64) {
        sqlx::query_as("SELECT p.is_active, EXTRACT(EPOCH FROM (m.review_due_at-now()))::float8/86400.0,
                               (SELECT count(*) FROM program_source_holds h WHERE h.program_id=p.id AND h.resolved_at IS NULL)
                        FROM programs p JOIN program_source_monitors m ON m.program_id=p.id WHERE p.id=$1")
            .bind(f.program_id).fetch_one(pool).await.unwrap()
    }

    async fn last(pool: &PgPool, f: &Fx) -> String {
        sqlx::query_scalar("SELECT outcome FROM corporate_evidence_checks WHERE candidate_id=$1 ORDER BY checked_at DESC, id LIMIT 1")
            .bind(f.candidate_id).fetch_optional(pool).await.unwrap().unwrap_or_default()
    }

    /// 30 days of hourly-capped scheduled runs and the overdue sweep, one
    /// simulated day at a time (all timestamps moved back a day).
    async fn thirty_days(pool: &PgPool, f: &Fx, mode: VerifyMode) -> Vec<String> {
        let mut log = Vec::new();
        for day in 0..30 {
            let picked = scheduled(pool, f, mode).await;
            crate::source_monitoring::hold_overdue_sources(pool, chrono::Duration::hours(72), 1_000).await.unwrap();
            let (active, due_days, holds) = state(pool, f).await;
            if day % 5 == 0 || !active {
                log.push(format!("day{day}: picked={picked} outcome={} active={active} due_in_days={due_days:.1} open_holds={holds}", last(pool, f).await));
            }
            if !active { break; }
            for sql in [
                "UPDATE program_source_monitors SET review_due_at=review_due_at-interval '1 day', verified_at=verified_at-interval '1 day' WHERE candidate_id=$1",
                "UPDATE corporate_evidence_targets SET last_checked_at=last_checked_at-interval '1 day', next_check_at=next_check_at-interval '1 day' WHERE candidate_id=$1",
            ] {
                sqlx::query(sql).bind(f.candidate_id).execute(pool).await.unwrap();
            }
        }
        log
    }

    async fn scenario(name: &str, mode: VerifyMode, mode_env: &str, page_html: String) {
        let Some(pool) = db().await else { eprintln!("TEST_DATABASE_URL unset; skipping"); return; };
        env(mode_env);
        let page: Page = Arc::new(Mutex::new((200, page_html)));
        let url = serve(page.clone()).await;
        let f = published(&pool, &url).await;
        for line in thirty_days(&pool, &f, mode).await {
            println!("R9BDB {name} {line}");
        }
        let (active, due, holds) = state(&pool, &f).await;
        println!("R9BDB {name} END active={active} due_in_days={due:.1} open_holds={holds}");
        cleanup(&pool, &f).await;
        clear();
    }

    #[tokio::test]
    async fn r9b_db_scenarios() {
        scenario("shadow/good", VerifyMode::Shadow, "shadow", good()).await;
        scenario("shadow/ended_one_signal", VerifyMode::Shadow, "shadow", ended_one_signal()).await;
        scenario("shadow/offer_removed", VerifyMode::Shadow, "shadow", removed()).await;
        scenario("hide_only/good", VerifyMode::HideOnly, "hide_only", good()).await;
        scenario("hide_only/ended_one_signal", VerifyMode::HideOnly, "hide_only", ended_one_signal()).await;
        scenario("hide_only/offer_removed", VerifyMode::HideOnly, "hide_only", removed()).await;
    }

    /// shadow/http_500 needs a 500 page: done here separately.
    #[tokio::test]
    async fn r9b_db_unreadable() {
        let Some(pool) = db().await else { return; };
        for (mode, m) in [(VerifyMode::Shadow, "shadow"), (VerifyMode::HideOnly, "hide_only")] {
            env(m);
            let page: Page = Arc::new(Mutex::new((500, "error".into())));
            let url = serve(page.clone()).await;
            let f = published(&pool, &url).await;
            for line in thirty_days(&pool, &f, mode).await {
                println!("R9BDB {m}/http_500 {line}");
            }
            let (active, due, holds) = state(&pool, &f).await;
            println!("R9BDB {m}/http_500 END active={active} due_in_days={due:.1} open_holds={holds}");
            cleanup(&pool, &f).await;
            clear();
        }
    }
}
