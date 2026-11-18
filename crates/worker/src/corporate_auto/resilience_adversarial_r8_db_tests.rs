//! Round-8 review, DB part (mounted under `evidence::tests`, reviewer
//! checkout only). hide_only must never make an offer visible. The hash
//! monitor (`source_monitoring`) still runs on evidence_auto sources in
//! hide_only (`evidence::live_enabled()` is false), and its unchanged-hash
//! path releases evidence holds.

use super::*;

async fn active(pool: &PgPool, f: &Fixture) -> bool {
    sqlx::query_scalar("SELECT is_active FROM programs WHERE id=$1").bind(f.program_id).fetch_one(pool).await.unwrap()
}

async fn holds(pool: &PgPool, f: &Fixture) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT hold_reason, resolution FROM program_source_holds WHERE program_id=$1 ORDER BY created_at, id")
        .bind(f.program_id).fetch_all(pool).await.unwrap()
}

async fn hash_check(pool: &PgPool, f: &Fixture, hash: &str) -> crate::source_monitoring::ContentChange {
    crate::source_monitoring::record_page_check(pool, crate::source_monitoring::PageCheck {
        candidate_id: f.candidate_id, previous_content_hash: Some(hash), content_hash: hash,
        page_title: Some("t"), review_text: None, etag: None, last_modified: None,
        http_status: 200, fetched_bytes: 1000, recheck_interval_seconds: 86_400, thin_content: false,
    }).await.unwrap()
}

#[tokio::test]
async fn r8_hide_only_hold_is_released_by_the_hash_monitor() {
    let Some(pool) = db().await else { eprintln!("TEST_DATABASE_URL unset; skipping"); return; };
    let _guard = DB_LOCK.lock().await;
    assert!(!live_enabled(), "hide_only/shadow: the hash monitor does not step aside");
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(active(&pool, &f).await, "shown after a live confirmation");
    // The hash monitor fetched the same page while it was current: its
    // baseline hash is stored on the monitor.
    let h = "a".repeat(32);
    let first = hash_check(&pool, &f, &h).await;
    println!("R8DB baseline {first:?}");
    // hide_only, allowlisted reason: a passed end date in the section.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>",
    );
    pass(&pool, &f, VerifyMode::HideOnly).await;
    let (outcome, applied) = last_outcome(&pool, &f).await;
    println!("R8DB hide_only outcome={outcome} applied={applied} active={}", active(&pool, &f).await);
    assert!(applied && !active(&pool, &f).await, "hide_only hid it");
    // The next hash fetch reads the same content hash (a page whose end date
    // simply passed reads the same text; here the hash is replayed).
    let second = hash_check(&pool, &f, &h).await;
    let shown = active(&pool, &f).await;
    println!("R8DB hash recheck {second:?} active_after={shown} holds={:?}", holds(&pool, &f).await);
    // A 304 takes the same path.
    pass(&pool, &f, VerifyMode::HideOnly).await;
    let rehidden = !active(&pool, &f).await;
    crate::source_monitoring::record_not_modified(&pool, f.candidate_id, Some(&h), 86_400).await.unwrap();
    println!("R8DB rehidden_by_next_evidence_pass={rehidden} active_after_304={}", active(&pool, &f).await);
    cleanup(&pool, &f).await;
    assert!(!shown, "BROKEN: the hash monitor made a hide_only-hidden offer visible again");
}

#[tokio::test]
async fn r8_hide_only_gone_hold_is_released_by_the_hash_monitor() {
    let Some(pool) = db().await else { eprintln!("TEST_DATABASE_URL unset; skipping"); return; };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    let h = "b".repeat(32);
    hash_check(&pool, &f, &h).await;
    *site.page.lock().unwrap() = (404, "not found".into());
    pass(&pool, &f, VerifyMode::HideOnly).await;
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at=now()-interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::HideOnly).await;
    let held = !active(&pool, &f).await;
    let gone_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar("SELECT first_gone_at FROM corporate_evidence_targets WHERE candidate_id=$1")
        .bind(f.candidate_id).fetch_one(&pool).await.unwrap();
    // The page is back after a deploy; the hash monitor reads the old hash.
    *site.page.lock().unwrap() = (200, nike_page("10", "30", "12"));
    let change = hash_check(&pool, &f, &h).await;
    let shown = active(&pool, &f).await;
    // Evidence on the recovered page in hide_only.
    pass(&pool, &f, VerifyMode::HideOnly).await;
    let gone_after: Option<chrono::DateTime<Utc>> = sqlx::query_scalar("SELECT first_gone_at FROM corporate_evidence_targets WHERE candidate_id=$1")
        .bind(f.candidate_id).fetch_one(&pool).await.unwrap();
    println!("R8DB gone: held={held} first_gone_after_hold={gone_at:?} hash={change:?} active_after_hash={shown} first_gone_after_recovery={gone_after:?} holds={:?}", holds(&pool, &f).await);
    cleanup(&pool, &f).await;
    assert!(held, "held after two 404s 24 h apart");
    assert!(!shown, "BROKEN: the hash monitor released the source_gone hold in hide_only");
}

/// An unknown allowlist key: the worker does not stop at startup; every
/// corporate job turns itself off with a WARN log.
#[test]
fn r8_unknown_hide_reason_disables_jobs_instead_of_stopping() {
    std::env::set_var("CORPORATE_AUTO_HIDE_REASONS", "section_date_past,sourc_gone");
    let flags = Flags::from_env();
    let live = live_enabled();
    std::env::remove_var("CORPORATE_AUTO_HIDE_REASONS");
    println!("R8DB unknown_key flags_err={} live_enabled={live}", flags.is_err());
    assert!(flags.is_err());
}
