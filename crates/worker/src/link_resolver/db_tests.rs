//! Repository tests on a throwaway database (`TEST_DATABASE_URL`).
use super::classify::{Observation, PageFacts};
use super::repository::{self, ApplySummary};
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

async fn programme(
    pool: &PgPool,
    source: &str,
    title: &str,
    official: Option<&str>,
    raw: Value,
) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let source_id = format!("link-test-{}", id.simple());
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,official_url,raw_payload)
         VALUES($1,'policy',$2,$3,$4,'active',true,$5,$6)",
    )
    .bind(id)
    .bind(source)
    .bind(&source_id)
    .bind(title)
    .bind(official)
    .bind(raw)
    .execute(pool)
    .await
    .unwrap();
    (id, source_id)
}

async fn snapshot(pool: &PgPool, source_id: &str, raw: Value) {
    sqlx::query(
        "INSERT INTO source_snapshots(id,source_name,source_id,content_hash,raw_payload,fetched_at,normalizer_version)
         VALUES($1,'gov_benefits',$2,'h',$3,now(),0)
         ON CONFLICT(source_name,source_id) DO UPDATE SET raw_payload=excluded.raw_payload",
    )
    .bind(Uuid::new_v4())
    .bind(source_id)
    .bind(raw)
    .execute(pool)
    .await
    .unwrap();
}

async fn rows(
    pool: &PgPool,
    program: Uuid,
) -> Vec<(String, String, String, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT field, url, check_status, kind, sub FROM program_link_candidates WHERE program_id=$1 ORDER BY field_rank, url",
    )
    .bind(program)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// (state, selected url, selected kind, notice url)
async fn selection(
    pool: &PgPool,
    program: Uuid,
) -> (String, Option<String>, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT s.state, c.url, c.kind, n.url FROM program_link_selections s
         LEFT JOIN program_link_candidates c ON c.id=s.selected_id
         LEFT JOIN program_link_candidates n ON n.id=s.notice_id WHERE s.program_id=$1",
    )
    .bind(program)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// An application form: six fields, no password.
fn form(final_url: &str, title: &str) -> Observation {
    Observation::Fetched {
        status: 200,
        final_url: final_url.into(),
        page: Some(PageFacts {
            title: title.into(),
            forms: 1,
            inputs: 6,
            ..PageFacts::default()
        }),
    }
}

fn page(final_url: &str, title: &str, links: &[(&str, &str)]) -> Observation {
    Observation::Fetched {
        status: 200,
        final_url: final_url.into(),
        page: Some(PageFacts {
            title: title.into(),
            apply_links: links
                .iter()
                .map(|(t, h)| (t.to_string(), h.to_string()))
                .collect(),
            ..PageFacts::default()
        }),
    }
}

fn gone(url: &str) -> Observation {
    Observation::Fetched {
        status: 404,
        final_url: url.into(),
        page: None,
    }
}

async fn apply(pool: &PgPool, url: &str, obs: &Observation) {
    repository::apply(pool, url, obs, &mut ApplySummary::default())
        .await
        .unwrap();
}

async fn cleanup(pool: &PgPool, ids: &[Uuid], source_ids: &[String]) {
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM source_snapshots WHERE source_name='gov_benefits' AND source_id=ANY($1)",
    )
    .bind(source_ids)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn sync_keeps_every_candidate_and_waits_for_their_checks() {
    let pool = pool().await;
    let n = Uuid::new_v4().simple().to_string();
    let notice = format!("https://www.t{n}.go.kr/board/view.do?idx=1");
    let raw = json!({
        "aplyUrlAddr": "https://forms.gle/closed1", "refUrlAddr1": notice, "plcyNo": "20261019005400214010",
        "plcyAplyMthdCn": "구글폼(https://docs.google.com/forms/d/e/x/viewform) 접수",
    });
    let (id, sid) = programme(
        &pool,
        "youth_center",
        "링크 테스트 청년 지원",
        Some(&notice),
        raw,
    )
    .await;
    let summary = repository::sync_only(&pool, Some(&[id]), 10, &[])
        .await
        .unwrap();
    assert_eq!((summary.examined, summary.changed), (1, 1));
    let stored = rows(&pool, id).await;
    let status: Vec<(&str, &str)> = stored
        .iter()
        .map(|r| (r.0.as_str(), r.2.as_str()))
        .collect();
    assert_eq!(
        status,
        vec![
            ("aplyUrlAddr", "unchecked"),
            ("refUrlAddr1", "unchecked"),
            // A form service from free text is never shown or fetched.
            ("plcyAplyMthdCn_text", "skipped"),
            ("youthcenter_detail", "skipped"),
        ]
    );
    // Not all links checked: the API keeps today's link.
    assert_eq!(selection(&pool, id).await.0, "pending");
    apply(&pool, &notice, &page(&notice, "청년 지원 공고", &[])).await;
    assert_eq!(selection(&pool, id).await.0, "pending");
    // The audit's closed Google form, by rule: a hard failure, never shown.
    let closed = Observation::Fetched {
        status: 200,
        final_url: "https://docs.google.com/forms/d/e/1FAIpQL/closedform".into(),
        page: None,
    };
    apply(&pool, "https://forms.gle/closed1", &closed).await;
    let (state, url, kind, _) = selection(&pool, id).await;
    assert_eq!(
        (state.as_str(), url.as_deref(), kind.as_deref()),
        ("selected", Some(notice.as_str()), Some("notice"))
    );
    // Unchanged inputs: nothing re-derived.
    let again = repository::sync_only(&pool, Some(&[id]), 10, &[])
        .await
        .unwrap();
    assert_eq!((again.examined, again.changed), (1, 0));
    cleanup(&pool, &[id], &[sid]).await;
}

#[tokio::test]
async fn confirmed_dead_links_hand_over_and_come_back_after_two_passes() {
    let pool = pool().await;
    let n = Uuid::new_v4().simple().to_string();
    let online = format!("https://apply.t{n}.go.kr/aply/reqst.do?id=1");
    let portal = format!("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/{n}");
    let raw = json!({"서비스ID": n, "상세조회URL": portal, "조회수": 1});
    let (id, sid) = programme(
        &pool,
        "gov_benefits",
        "링크 테스트 보조금",
        Some(&portal),
        raw.clone(),
    )
    .await;
    let mut with_detail = raw.clone();
    with_detail["detail"] = json!({"온라인신청사이트URL": online, "접수기관명": "주민센터"});
    snapshot(&pool, &sid, with_detail).await;
    repository::sync_only(&pool, Some(&[id]), 10, &[])
        .await
        .unwrap();
    apply(&pool, &online, &form(&online, "온라인 신청")).await;
    assert_eq!(selection(&pool, id).await.2.as_deref(), Some("application"));
    // One hard failure keeps the selected link until it is confirmed.
    apply(&pool, &online, &gone(&online)).await;
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(online.as_str())
    );
    // A second failure within a day confirms nothing.
    apply(&pool, &online, &gone(&online)).await;
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(online.as_str())
    );
    sqlx::query(
        "UPDATE program_link_candidates SET first_fail_at=now()-interval '25 hours' WHERE url=$1",
    )
    .bind(&online)
    .execute(&pool)
    .await
    .unwrap();
    apply(&pool, &online, &gone(&online)).await;
    let (_, url, kind, _) = selection(&pool, id).await;
    assert_eq!(
        (url.as_deref(), kind.as_deref()),
        (Some(portal.as_str()), Some("information"))
    );
    apply(&pool, &online, &form(&online, "온라인 신청")).await;
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(portal.as_str())
    );
    apply(&pool, &online, &form(&online, "온라인 신청")).await;
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(online.as_str())
    );
    // A failed detail read keeps the detail's candidate.
    snapshot(&pool, &sid, raw).await;
    repository::sync_only(&pool, Some(&[id]), 10, &[])
        .await
        .unwrap();
    assert!(rows(&pool, id).await.iter().any(|r| r.1 == online));
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(online.as_str())
    );
    cleanup(&pool, &[id], &[sid]).await;
}

#[tokio::test]
async fn an_application_link_on_the_notice_page_is_promoted_after_its_own_check() {
    let pool = pool().await;
    let n = Uuid::new_v4().simple().to_string();
    let notice = format!("https://www.h{n}.go.kr/bbs/view.do?idx=7");
    let target = format!("https://www.h{n}.go.kr/youth/aplyReqst.do?id=7");
    let raw = json!({"refUrlAddr1": notice});
    let (id, sid) = programme(
        &pool,
        "youth_center",
        "링크 테스트 청년 공간",
        Some(&notice),
        raw,
    )
    .await;
    repository::sync_only(&pool, Some(&[id]), 10, &[])
        .await
        .unwrap();
    let links = [
        ("온라인 신청하기", "/youth/aplyReqst.do?id=7"),
        ("민원 신청", "/minwon/apply.do"),
    ];
    apply(
        &pool,
        &notice,
        &page(&notice, "청년 공간 이용 공고", &links),
    )
    .await;
    let stored = rows(&pool, id).await;
    let hop = stored
        .iter()
        .find(|r| r.0 == super::candidates::ONE_HOP_FIELD)
        .expect("one-hop candidate");
    assert_eq!(
        (hop.1.as_str(), hop.2.as_str()),
        (target.as_str(), "unchecked")
    );
    assert_eq!(
        stored
            .iter()
            .filter(|r| r.0 == super::candidates::ONE_HOP_FIELD)
            .count(),
        1
    );
    // An unchecked one-hop link does not hold the selection back.
    let (state, url, _, _) = selection(&pool, id).await;
    assert_eq!(
        (state.as_str(), url.as_deref()),
        ("selected", Some(notice.as_str()))
    );
    apply(&pool, &target, &form(&target, "청년 공간 이용 신청")).await;
    let (_, url, kind, notice_url) = selection(&pool, id).await;
    assert_eq!(
        (url.as_deref(), kind.as_deref(), notice_url.as_deref()),
        (
            Some(target.as_str()),
            Some("application"),
            Some(notice.as_str())
        )
    );
    // The page drops the link: the candidate goes with it.
    apply(&pool, &notice, &page(&notice, "청년 공간 이용 공고", &[])).await;
    assert!(!rows(&pool, id)
        .await
        .iter()
        .any(|r| r.0 == super::candidates::ONE_HOP_FIELD));
    assert_eq!(
        selection(&pool, id).await.1.as_deref(),
        Some(notice.as_str())
    );
    cleanup(&pool, &[id], &[sid]).await;
}

#[tokio::test]
async fn resolver_tables_are_service_only() {
    let pool = pool().await;
    for table in [
        "program_link_candidates",
        "program_link_selections",
        "link_check_hosts",
        "link_resolver_metrics",
    ] {
        let (rls, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE oid=$1::regclass",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(rls && forced, "{table}");
        let policies: Vec<String> =
            sqlx::query_scalar("SELECT policyname::text FROM pg_policies WHERE tablename=$1")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(policies, vec![format!("{table}_all_service")], "{table}");
    }
}

#[tokio::test]
async fn daily_metrics_mark_a_drop_in_application_and_notice_links_degraded() {
    let pool = pool().await;
    let first = super::metrics::record(&pool).await.unwrap();
    let sources = first["sources"].as_array().unwrap().clone();
    let Some(source) = sources
        .first()
        .and_then(|s| s["source_type"].as_str())
        .map(str::to_string)
    else {
        return;
    };
    // Yesterday every programme of the source had an application link.
    sqlx::query(
        "INSERT INTO link_resolver_metrics(day,source_type,programmes,application,notice,information,portal,failing,no_link,pending,candidates,broken_candidates)
         VALUES((now() AT TIME ZONE 'Asia/Seoul')::date - 1,$1,10,10,0,0,0,0,0,0,0,0)
         ON CONFLICT(day,source_type) DO UPDATE SET programmes=10,application=10,notice=0",
    )
    .bind(&source)
    .execute(&pool)
    .await
    .unwrap();
    let details = super::metrics::record(&pool).await.unwrap();
    assert_eq!(details["degraded"], true, "{details}");
    assert!(details["reason"].as_str().unwrap().contains(&source));
    sqlx::query("DELETE FROM link_resolver_metrics WHERE day=(now() AT TIME ZONE 'Asia/Seoul')::date - 1 AND source_type=$1")
        .bind(&source)
        .execute(&pool)
        .await
        .unwrap();
}
