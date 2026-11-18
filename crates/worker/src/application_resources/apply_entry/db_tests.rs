//! Repository tests on a throwaway database (`TEST_DATABASE_URL`).
use super::readers::Reader;
use super::repository::{self, Recorded};
use super::{Failed, Outcome, Passed, Programme};
use mazimi_core::application_resources::{fingerprint, Resource, AUTOMATED_REVIEWER};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("isolated test database is required");
    sqlx::migrate!("../../infra/migrations")
        .run(&pool)
        .await
        .expect("migrations apply");
    pool
}

async fn programme(pool: &PgPool, method: &str, raw: Value) -> Programme {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,official_url,raw_payload,application_method)
         VALUES($1,'policy','youth_center',$2,'청년 학자금 신용회복 지원','active',true,
                'https://youth.seoul.go.kr/youthConts.do?key=1',$3,$4)",
    )
    .bind(id)
    .bind(format!("apply-test-{}", id.simple()))
    .bind(raw)
    .bind(method)
    .execute(pool)
    .await
    .unwrap();
    load(pool, id).await
}

async fn load(pool: &PgPool, id: Uuid) -> Programme {
    repository::due(pool, 100_000)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.program.id == id)
        .expect("programme is due")
}

fn passed(url: &str, final_url: &str) -> Outcome {
    Outcome::Passed(Passed {
        reader: Reader::Generic,
        url: url.into(),
        label: "신청하기".into(),
        source_url: "https://youth.seoul.go.kr/youthConts.do?key=1".into(),
        depth: 1,
        why: "content+form+button".into(),
        access: "direct",
        step: "application_form",
        final_url: final_url.into(),
        source_hash: mazimi_core::application_resources::digest(url.as_bytes()),
        content_hash: mazimi_core::application_resources::digest(final_url.as_bytes()),
    })
}

fn failed(url: &str, code: &str) -> Outcome {
    Outcome::Failed(Failed {
        reader: Reader::Generic,
        url: url.into(),
        label: "신청하기".into(),
        source_url: "https://youth.seoul.go.kr/youthConts.do?key=1".into(),
        depth: 1,
        why: "content".into(),
        code: code.into(),
    })
}

async fn rows(pool: &PgPool, program: Uuid) -> Vec<Resource> {
    sqlx::query_as(
        "SELECT * FROM program_application_resources WHERE program_id=$1 ORDER BY created_at, url",
    )
    .bind(program)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn scan(pool: &PgPool, program: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT outcome, reason FROM program_apply_link_scans WHERE program_id=$1")
        .bind(program)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn record(pool: &PgPool, p: &Programme, outcome: &Outcome) -> Recorded {
    let mut counts = Recorded::default();
    repository::record(pool, p, outcome, chrono::Duration::zero(), &mut counts)
        .await
        .unwrap();
    counts
}

const APPLY: &str = "https://youth.seoul.go.kr/loanCrdtRcvr/step1.do?key=1";

#[tokio::test]
async fn a_link_is_verified_only_by_a_second_consistent_check() {
    let pool = pool().await;
    let p = programme(&pool, "온라인", json!({})).await;
    let first = record(&pool, &p, &passed(APPLY, APPLY)).await;
    assert_eq!(first.inserted, 1);
    let r = &rows(&pool, p.program.id).await[0];
    assert_eq!(
        (r.status.as_str(), r.kind.as_str()),
        ("candidate", "application")
    );
    assert!(!r.usable(&fingerprint(&p.program), chrono::Utc::now()));
    assert_eq!(scan(&pool, p.program.id).await.0, "pending");

    // A different target at the second check restarts the clock.
    let changed = record(&pool, &p, &passed(APPLY, "https://youth.seoul.go.kr/other")).await;
    assert_eq!(changed.restarted, 1);
    assert_eq!(rows(&pool, p.program.id).await[0].status, "candidate");

    let p = load_any(&pool, p.program.id).await;
    let second = record(&pool, &p, &passed(APPLY, "https://youth.seoul.go.kr/other")).await;
    assert_eq!(second.verified, 1);
    let r = &rows(&pool, p.program.id).await[0];
    assert_eq!(r.status, "verified");
    assert_eq!(r.reviewed_by, Some(AUTOMATED_REVIEWER));
    assert_eq!(r.access, "direct");
    assert!(r.usable(&fingerprint(&p.program), chrono::Utc::now()));
    let window = r.valid_until.unwrap() - r.verified_at.unwrap();
    assert_eq!(window, chrono::Duration::days(7));
    let reviews: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_resource_reviews WHERE resource_id=$1 AND decision='verified' AND reviewer_id=$2",
    )
    .bind(r.id)
    .bind(AUTOMATED_REVIEWER)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reviews, 1);
    assert_eq!(scan(&pool, p.program.id).await.0, "verified");

    // A later consistent check renews it.
    let renewed = record(&pool, &p, &passed(APPLY, "https://youth.seoul.go.kr/other")).await;
    assert_eq!(renewed.renewed, 1);

    // The official page now points elsewhere: the old link is superseded.
    let moved = "https://youth.seoul.go.kr/loanCrdtRcvr/step1.do?key=2";
    let next = record(&pool, &p, &passed(moved, moved)).await;
    assert_eq!((next.inserted, next.superseded), (1, 1));
    let all = rows(&pool, p.program.id).await;
    let old = all.iter().find(|r| r.url == APPLY).unwrap();
    assert_eq!(old.status, "rejected");
    assert_eq!(old.check_error.as_deref(), Some("auto_rejected:superseded"));
    assert!(old.valid_until.is_none());
}

/// The scan row may not be due yet; tests read the programme regardless.
async fn load_any(pool: &PgPool, id: Uuid) -> Programme {
    sqlx::query("UPDATE program_apply_link_scans SET next_scan_at=now() WHERE program_id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    load(pool, id).await
}

#[tokio::test]
async fn two_hard_failures_unpublish_a_verified_link_and_candidates_fail_at_once() {
    let pool = pool().await;
    let p = programme(&pool, "온라인", json!({})).await;
    record(&pool, &p, &passed(APPLY, APPLY)).await;
    record(&pool, &p, &passed(APPLY, APPLY)).await;
    assert_eq!(rows(&pool, p.program.id).await[0].status, "verified");
    let once = record(&pool, &p, &failed(APPLY, "http_404")).await;
    assert_eq!(once.demoted, 0);
    let r = &rows(&pool, p.program.id).await[0];
    assert_eq!((r.status.as_str(), r.consecutive_gone), ("verified", 1));
    let twice = record(&pool, &p, &failed(APPLY, "http_404")).await;
    assert_eq!(twice.demoted, 1);
    let r = &rows(&pool, p.program.id).await[0];
    assert_eq!(r.status, "rejected");
    assert_eq!(r.check_error.as_deref(), Some("auto_rejected:http_404"));
    // A target that never passed is stored rejected with its reason.
    let q = programme(&pool, "온라인", json!({})).await;
    let home = "https://youth.seoul.go.kr/main.do";
    let counts = record(&pool, &q, &failed(home, "site_home")).await;
    assert_eq!(counts.rejected, 1);
    let r = &rows(&pool, q.program.id).await[0];
    assert_eq!(
        (r.status.as_str(), r.access.as_str()),
        ("rejected", "unknown")
    );
    assert_eq!(
        scan(&pool, q.program.id).await,
        ("rejected".into(), Some("site_home".into()))
    );
}

#[tokio::test]
async fn human_decisions_are_never_overridden() {
    let pool = pool().await;
    let admin = Uuid::new_v4();
    // A person rejected this URL: the job does not bring it back.
    let p = programme(&pool, "온라인", json!({})).await;
    sqlx::query(
        "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,status,program_fingerprint,reviewed_by,review_note)
         VALUES($1,'application',$2,$2,'신청','rejected',$3,$4,'사람이 확인한 결과 다른 사업의 신청 화면입니다.')",
    )
    .bind(p.program.id)
    .bind(APPLY)
    .bind(fingerprint(&p.program))
    .bind(admin)
    .execute(&pool)
    .await
    .unwrap();
    for _ in 0..2 {
        record(&pool, &p, &passed(APPLY, APPLY)).await;
    }
    let r = &rows(&pool, p.program.id).await[0];
    assert_eq!(
        (r.status.as_str(), r.reviewed_by),
        ("rejected", Some(admin))
    );
    assert_eq!(
        scan(&pool, p.program.id).await.1.as_deref(),
        Some("human_rejected")
    );

    // A person verified another link: the job leaves the programme alone.
    let q = programme(&pool, "온라인", json!({})).await;
    sqlx::query(
        "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,status,access,program_fingerprint,
           verified_at,valid_until,reviewed_by,review_note)
         VALUES($1,'application','https://youth.seoul.go.kr/human',$2,'신청','verified','direct',$3,now(),now()+interval '1 day',$4,
           '담당자가 직접 신청 화면을 확인했습니다. 추가 메모.')",
    )
    .bind(q.program.id)
    .bind(APPLY)
    .bind(fingerprint(&q.program))
    .bind(admin)
    .execute(&pool)
    .await
    .unwrap();
    let counts = record(&pool, &q, &passed(APPLY, APPLY)).await;
    assert_eq!((counts.human_kept, counts.inserted), (1, 0));
    assert_eq!(rows(&pool, q.program.id).await.len(), 1);
}

#[tokio::test]
async fn offline_programmes_get_no_link_and_wait_until_their_inputs_change() {
    let pool = pool().await;
    let p = programme(&pool, "방문, 전화", json!({})).await;
    let session = super::fetch::Session::new(pool.clone(), "test".into());
    let outcome = super::evaluate(&session, None, &p).await;
    assert_eq!(
        outcome,
        Outcome::Offline {
            reason: "offline_method"
        }
    );
    // An offline programme also loses a link it had.
    record(&pool, &p, &passed(APPLY, APPLY)).await;
    record(&pool, &p, &outcome).await;
    assert_eq!(rows(&pool, p.program.id).await[0].status, "rejected");
    assert_eq!(scan(&pool, p.program.id).await.0, "offline");
    let due = |pool: PgPool, id: Uuid| async move {
        repository::due(&pool, 100_000)
            .await
            .unwrap()
            .iter()
            .any(|p| p.program.id == id)
    };
    assert!(
        !due(pool.clone(), p.program.id).await,
        "offline waits 30 days"
    );
    // The source now says online: the changed inputs bring it back at once.
    sqlx::query("UPDATE programs SET application_method='방문, 인터넷' WHERE id=$1")
        .bind(p.program.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(due(pool.clone(), p.program.id).await);
}

#[tokio::test]
async fn the_scan_table_is_service_only_and_the_old_probe_skips_reader_rows() {
    let pool = pool().await;
    let (rls, forced): (bool, bool) = sqlx::query_as(
        "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname='program_apply_link_scans'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rls && forced);
    let policies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_policies WHERE tablename='program_apply_link_scans'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(policies, 1);
    let source = include_str!("../repository.rs");
    assert!(source.contains("AND r.reader IS NULL AND {available}"));
}

/// Review of #13: a site that keeps refusing (403, robots, network) is not
/// asked again daily forever; its programmes back off 1, 2, 4, then 7 days,
/// and the count restarts once a scan reads the page.
#[tokio::test]
async fn unreadable_programmes_back_off_and_restart_after_a_read() {
    let pool = pool().await;
    let p = programme(&pool, "온라인", json!({})).await;
    let unreachable = Outcome::Unreachable {
        reader: Reader::Generic,
        code: "http_403".into(),
    };
    let wait_days = |pool: PgPool, id: Uuid| async move {
        let (secs, n): (f64, i32) = sqlx::query_as(
            "SELECT extract(epoch FROM next_scan_at - scanned_at)::float8, consecutive_unreachable
             FROM program_apply_link_scans WHERE program_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        ((secs / 86_400.0).round() as i64, n)
    };
    let mut seen = vec![];
    for _ in 0..5 {
        record(&pool, &p, &unreachable).await;
        seen.push(wait_days(pool.clone(), p.program.id).await);
    }
    assert_eq!(seen, vec![(1, 1), (2, 2), (4, 3), (7, 4), (7, 5)]);
    record(
        &pool,
        &p,
        &Outcome::NotFound {
            reader: Reader::Generic,
            reason: "no_apply_element".into(),
        },
    )
    .await;
    assert_eq!(wait_days(pool.clone(), p.program.id).await, (14, 0));
    record(&pool, &p, &unreachable).await;
    assert_eq!(wait_days(pool.clone(), p.program.id).await, (1, 1));
}

/// The owner's robots decision, per caller, on the same cached answers:
/// the apply-link reader reads government hosts under RFC 9309 (an HTML
/// answer or a 4xx = no robots.txt), every other host and the link
/// resolver stay strict (HTML = bot wall, 401/403 = disallow), and 5xx
/// fails closed everywhere. The hosts below do not resolve, so a request
/// that passes robots ends in a DNS/connect error, never a fetch.
#[tokio::test]
async fn robots_policy_is_rfc9309_on_government_hosts_for_the_reader_only() {
    use crate::application_resources::apply_entry::fetch::{Fetched, Session};
    use crate::link_resolver::{
        check::{self, Pacer},
        classify::Observation,
    };
    let pool = pool().await;
    let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
    let host = |kind: &str, gov: bool| {
        if gov {
            format!("{kind}-{tag}.apply-robots-test.go.kr")
        } else {
            format!("{kind}-{tag}.apply-robots-test.or.kr")
        }
    };
    let seed = |origin: String, status: &'static str, code: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO link_check_hosts(origin,robots_status,failure_code,fetched_at) VALUES($1,$2,$3,now())",
            )
            .bind(origin)
            .bind(status)
            .bind(code)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    let cases = [
        // (answer as cached, government host?)
        ("html", "unreachable", "robots_html"),
        ("wall", "disallow_all", "robots_http_403"),
        ("down", "unreachable", "robots_http_503"),
    ];
    for (kind, status, code) in cases {
        for gov in [true, false] {
            seed(format!("https://{}", host(kind, gov)), status, code).await;
        }
    }
    let session = Session::new(pool.clone(), "Mazimi-Bot-test".into());
    let reader = |kind: &str, gov: bool| {
        let (session, url) = (
            session.clone(),
            format!("https://{}/apply", host(kind, gov)),
        );
        async move { session.get(&url).await }
    };
    let passed_robots = |f: &Fetched| matches!(f, Fetched::Error { code } if matches!(code.as_str(), "dns" | "connect" | "unsafe" | "timeout"));
    // Reader, government host: HTML and 403 mean no robots.txt; 503 fails closed.
    assert!(passed_robots(&reader("html", true).await));
    assert!(passed_robots(&reader("wall", true).await));
    assert_eq!(
        reader("down", true).await,
        Fetched::Error {
            code: "robots_http_503".into()
        }
    );
    // Reader, other host: strict.
    assert_eq!(
        reader("html", false).await,
        Fetched::Error {
            code: "robots_html".into()
        }
    );
    assert_eq!(reader("wall", false).await, Fetched::Blocked);
    // The link resolver stays strict on government hosts too.
    let pacer = Pacer::default();
    let resolver = |kind: &str| {
        let (pool, pacer, url) = (
            pool.clone(),
            pacer.clone(),
            format!("https://{}/apply", host(kind, true)),
        );
        async move { check::check(&pool, &url, "Mazimi-Bot-test", &pacer).await }
    };
    assert_eq!(
        resolver("html").await,
        Observation::Error {
            code: "robots_html".into()
        }
    );
    assert_eq!(resolver("wall").await, Observation::Blocked);
    assert_eq!(
        resolver("down").await,
        Observation::Error {
            code: "robots_http_503".into()
        }
    );
}
