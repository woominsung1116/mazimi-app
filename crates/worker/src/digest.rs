//! Daily operator digest (Korean), sent at 09:00 KST even when everything is
//! healthy, so silence itself becomes a signal. Counts and operator messages
//! only: no user rows, no personal data.

use anyhow::Result;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;

pub fn kst(at: DateTime<Utc>) -> String {
    let offset = FixedOffset::east_opt(9 * 3600).expect("valid offset");
    at.with_timezone(&offset).format("%m-%d %H:%M").to_string()
}

pub fn kst_hour(at: DateTime<Utc>) -> u32 {
    use chrono::Timelike;
    let offset = FixedOffset::east_opt(9 * 3600).expect("valid offset");
    at.with_timezone(&offset).hour()
}

pub struct Digest {
    pub subject: String,
    pub text: String,
    pub summary: Value,
}

#[derive(sqlx::FromRow)]
struct JobRow {
    job_name: String,
    status: String,
    last_success_at: Option<DateTime<Utc>>,
    details: Value,
    stale: bool,
}

/// Condition-review health: how many "조건 확인됨" reviews still match the
/// current notice text and are served.
async fn verified_health(pool: &PgPool) -> Result<Value> {
    let rows: Vec<(String, String, String, Option<Value>, bool)> = sqlx::query_as(
        "SELECT r.source_type, r.status, r.text_hash, p.raw_payload, c.id IS NOT NULL
         FROM program_condition_reviews r
         LEFT JOIN programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         LEFT JOIN current_programs c ON c.id = p.id",
    )
    .fetch_all(pool)
    .await?;
    let mut stale = 0;
    let mut complete_fresh_served = 0;
    let mut complete = 0;
    for (source_type, status, hash, payload, served) in &rows {
        let fresh = payload
            .as_ref()
            .and_then(|payload| mazimi_core::condition_review::text_hash(source_type, payload))
            .is_some_and(|current| &current == hash);
        if !fresh {
            stale += 1;
        }
        if status == "complete" {
            complete += 1;
            if fresh && *served {
                complete_fresh_served += 1;
            }
        }
    }
    Ok(json!({
        "reviews": rows.len(),
        "complete": complete,
        "stale": stale,
        "complete_fresh_served": complete_fresh_served,
    }))
}

pub async fn build(pool: &PgPool, now: DateTime<Utc>) -> Result<Digest> {
    let jobs = sqlx::query_as::<_, JobRow>(
        "SELECT job_name, status, last_success_at, details,
                last_success_at IS NULL OR last_success_at < now() - interval '26 hours' AS stale
         FROM worker_daily_jobs WHERE job_name LIKE 'daily:%' ORDER BY job_name",
    )
    .fetch_all(pool)
    .await?;
    let per_source: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT p.source_type, count(*), count(c.id)
         FROM programs p LEFT JOIN current_programs c ON c.id = p.id
         GROUP BY 1 ORDER BY 3 DESC, 1",
    )
    .fetch_all(pool)
    .await?;
    let db = crate::storage::measure_db(pool).await?;
    let disk_free_gb = crate::storage::host_disk_free_gb();
    let open_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT severity, count(*) FROM operational_events WHERE status <> 'resolved'
         GROUP BY 1",
    )
    .fetch_all(pool)
    .await?;
    let top_open: Vec<(String, String, String, i32)> = sqlx::query_as(
        "SELECT severity, component, message, occurrence_count FROM operational_events
         WHERE status <> 'resolved'
         ORDER BY CASE severity WHEN 'critical' THEN 3 WHEN 'error' THEN 2
                  WHEN 'warning' THEN 1 ELSE 0 END DESC, last_occurred_at DESC
         LIMIT 10",
    )
    .fetch_all(pool)
    .await?;
    let healing: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT event_type, message, occurrence_count FROM operational_events
         WHERE component = 'self_healing' AND last_occurred_at > now() - interval '1 day'
         ORDER BY last_occurred_at DESC LIMIT 20",
    )
    .fetch_all(pool)
    .await?;
    let approvals: (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE approval_expires_at <= now() + interval '7 days'),
                count(*)
         FROM benefit_source_domains
         WHERE is_active AND crawl_allowed AND compliance_status = 'approved'
           AND approval_expires_at > now() AND approval_expires_at <= now() + interval '30 days'",
    )
    .fetch_one(pool)
    .await?;
    // Parts of a source that stay unsettled with nothing shown: no alert,
    // one line here (`ingestion_parts`).
    let blocked_parts: Vec<(String, String, Option<String>, i32)> = sqlx::query_as(
        "SELECT source_name, id_prefix, reason_code, consecutive_unsettled_runs FROM ingestion_parts
         WHERE state = 'unsettled' AND shown = 0
           -- only parts the source still reports: those of its latest run
           AND last_run_id = (SELECT p2.last_run_id FROM ingestion_parts p2
                              WHERE p2.source_name = ingestion_parts.source_name
                              ORDER BY p2.updated_at DESC LIMIT 1)
         ORDER BY source_name, id_prefix",
    )
    .fetch_all(pool)
    .await?;
    let verified = verified_health(pool).await.ok();
    let corporate_audit = crate::corporate_auto::weekly_audit::aggregate(pool, None)
        .await
        .ok();

    let open = |severity: &str| {
        open_counts
            .iter()
            .find(|(s, _)| s == severity)
            .map_or(0, |(_, n)| *n)
    };
    let errors = open("critical") + open("error");
    let bad_jobs: Vec<&JobRow> = jobs
        .iter()
        .filter(|job| job.stale || matches!(job.status.as_str(), "failed" | "degraded"))
        .filter(|job| job.status != "disabled")
        .collect();
    let state = if errors > 0 || !bad_jobs.is_empty() {
        "확인 필요"
    } else {
        "정상"
    };

    let mut text = format!(
        "마지미 운영 일일 요약 ({} KST 기준)\n상태: {state}. 열린 오류 {errors}건, 문제 있는 수집 작업 {}건.\n",
        kst(now),
        bad_jobs.len()
    );
    text.push_str("\n■ 수집 작업\n");
    for job in &jobs {
        let label = match job.status.as_str() {
            "success" => "완료",
            "degraded" => "성공했지만 결과 이상",
            "failed" => "실패",
            "disabled" => "꺼짐(설정 없음)",
            "running" => "진행 중",
            other => other,
        };
        let last = job
            .last_success_at
            .map(kst)
            .unwrap_or_else(|| "없음".into());
        let stale = if job.stale && job.status != "disabled" {
            " · 26시간 내 성공 없음"
        } else {
            ""
        };
        let reason = job
            .details
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| format!(" · {reason}"))
            .unwrap_or_default();
        text.push_str(&format!(
            "- {}: {label} (최근 성공 {last}){stale}{reason}\n",
            job.job_name
        ));
    }
    text.push_str("\n■ 소스별 공고 수 (서비스 중 / 전체)\n");
    for (source, total, current) in &per_source {
        text.push_str(&format!("- {source}: {current} / {total}\n"));
    }
    text.push_str("\n■ 저장소\n");
    text.push_str(&format!("- DB 크기: {} MB\n", db.database_mb));
    if let Some((name, mb)) = db.top_tables_mb.first() {
        text.push_str(&format!("- 가장 큰 테이블: {name} {mb} MB\n"));
    }
    match disk_free_gb {
        Some(free) => text.push_str(&format!("- 서버 디스크 여유: {free} GB\n")),
        None => text.push_str("- 서버 디스크 여유: 측정 불가\n"),
    }
    text.push_str(&format!(
        "\n■ 열린 운영 이벤트: 심각 {} · 오류 {} · 경고 {} · 정보 {}\n",
        open("critical"),
        open("error"),
        open("warning"),
        open("info")
    ));
    for (severity, component, message, count) in &top_open {
        text.push_str(&format!(
            "- [{}][{component}] {message} ({count}회)\n",
            severity.to_ascii_uppercase()
        ));
    }
    text.push_str("\n■ 자동 조치 (최근 24시간)\n");
    if healing.is_empty() {
        text.push_str("- 없음\n");
    }
    for (event_type, message, count) in &healing {
        text.push_str(&format!("- {event_type}: {message} ({count}회)\n"));
    }
    if !blocked_parts.is_empty() {
        text.push_str(&format!(
            "\n■ 막힌 수집 범위 (보여주는 공고 없음): {}\n",
            blocked_parts
                .iter()
                .map(|(source, prefix, code, runs)| format!(
                    "{source} {prefix}* {} {runs}회 연속",
                    code.as_deref().unwrap_or("")
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    text.push_str(&format!(
        "\n■ 도메인 수집 승인 만료 예정: 30일 안 {}건, 7일 안 {}건\n",
        approvals.1, approvals.0
    ));
    if let Some(verified) = &verified {
        text.push_str(&format!(
            "\n■ 조건 확인됨 검수: 전체 {} · complete {} · 원문 변경(stale) {} · 서비스 중 유효 complete {}\n",
            verified["reviews"], verified["complete"], verified["stale"], verified["complete_fresh_served"]
        ));
    }
    if let Some(audit) = &corporate_audit {
        text.push_str(&crate::corporate_auto::weekly_audit::digest_text(audit));
    }

    let subject = format!("[마지미 운영] 일일 요약 · {state} · 오류 {errors}건");
    let summary = json!({
        "state": state,
        "open_errors": errors,
        "problem_jobs": bad_jobs.iter().map(|job| job.job_name.clone()).collect::<Vec<_>>(),
        "database_mb": db.database_mb,
        "host_disk_free_gb": disk_free_gb,
        "self_healing_actions": healing.len(),
        "approvals_expiring_30d": approvals.1,
        "verified": verified,
        "corporate_weekly_audit": corporate_audit.as_ref().map(|audit| json!({
            "audited": audit["audited"], "agrees": audit["agrees"],
            "disagrees": audit["disagrees"], "unreadable": audit["unreadable"],
            "agree_rate": audit["agree_rate"],
            "disagreement_by_field": audit["disagreement_by_field"],
        })),
    });
    Ok(Digest {
        subject,
        text,
        summary,
    })
}

/// Sends through one channel at most once per Korean day. A per-channel
/// marker row (`marker:ops_digest:<channel>`) records the day's success, so
/// a retry after a partial failure only resends the failed channel.
pub async fn deliver_once<F, Fut>(pool: &PgPool, channel: &str, send: F) -> Result<bool>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let marker = format!("marker:ops_digest:{channel}");
    let sent_today: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM worker_daily_jobs WHERE job_name = $1
           AND last_success_at >= (date_trunc('day', now() AT TIME ZONE 'Asia/Seoul') AT TIME ZONE 'Asia/Seoul'))",
    )
    .bind(&marker)
    .fetch_one(pool)
    .await?;
    if sent_today {
        return Ok(false);
    }
    send().await?;
    sqlx::query(
        "INSERT INTO worker_daily_jobs(job_name, last_success_at, status) VALUES ($1, now(), 'success')
         ON CONFLICT (job_name) DO UPDATE SET last_success_at = now(), status = 'success'",
    )
    .bind(&marker)
    .execute(pool)
    .await?;
    Ok(true)
}

/// Builds and delivers the digest over every configured channel. Returns the
/// job details; `disabled` when no channel is configured (the digest is still
/// recorded in the details for the admin page). A failed channel fails the
/// job (hourly retry) without resending a channel that already succeeded.
pub async fn send(pool: &PgPool, now: DateTime<Utc>) -> Result<Value> {
    let digest = build(pool, now).await?;
    let mut configured = Vec::new();
    let mut failed = Vec::new();
    if let Some(email) = crate::ops_email::EmailConfig::from_env()? {
        configured.push("email");
        let html = crate::ops_email::html_from_text(&digest.subject, &digest.text);
        if let Err(error) = deliver_once(pool, "email", || async {
            email.send(&digest.subject, &digest.text, &html).await
        })
        .await
        {
            tracing::warn!(error = %error, "digest email failed");
            failed.push("email");
        }
    }
    if crate::operations::webhook_configured() {
        configured.push("webhook");
        if let Err(error) = deliver_once(pool, "webhook", || async {
            crate::operations::post_webhook_text_from_env(&digest.text)
                .await
                .map(|_| ())
        })
        .await
        {
            tracing::warn!(error = %error, "digest webhook failed");
            failed.push("webhook");
        }
    }
    anyhow::ensure!(
        failed.is_empty(),
        "digest delivery failed for {}; retried without resending the others",
        failed.join(", ")
    );
    let mut details = json!({"digest": digest.summary, "channels": configured});
    if configured.is_empty() {
        details["disabled"] = json!(true);
        details["reason"] = json!("OPS_ALERT_EMAIL_TO and OPS_ALERT_WEBHOOK_URL are not set");
    }
    Ok(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kst_formatting_and_hour() {
        let at: DateTime<Utc> = "2026-10-17T00:05:00Z".parse().unwrap();
        assert_eq!(kst(at), "10-17 09:05");
        assert_eq!(kst_hour(at), 9);
    }

    /// A channel that succeeded today is not resent on retry; a failed one
    /// is.
    #[tokio::test]
    async fn a_digest_channel_is_sent_at_most_once_a_day() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required");
        let channel = format!("test_{}", uuid::Uuid::new_v4().simple());
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt = |fail: bool| {
            let sends = sends.clone();
            move || async move {
                sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if fail {
                    anyhow::bail!("smtp send failed (transient)")
                }
                Ok(())
            }
        };
        assert!(deliver_once(&pool, &channel, attempt(true)).await.is_err());
        assert!(deliver_once(&pool, &channel, attempt(false)).await.unwrap());
        assert!(!deliver_once(&pool, &channel, attempt(false)).await.unwrap());
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 2);
        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(format!("marker:ops_digest:{channel}"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn digest_reports_jobs_sources_storage_and_events_without_user_data() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required");
        let name = format!("daily:digest_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO worker_daily_jobs(job_name,status,last_success_at,details)
             VALUES($1,'degraded',now(),'{\"reason\":\"all_closed\"}')",
        )
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
        let digest = build(&pool, Utc::now()).await.unwrap();
        assert!(digest
            .text
            .contains(&format!("- {name}: 성공했지만 결과 이상")));
        assert!(digest.text.contains("■ 저장소") && digest.text.contains("DB 크기"));
        assert!(digest.text.contains("■ 열린 운영 이벤트"));
        assert_eq!(digest.summary["state"], "확인 필요");
        assert!(!digest.text.contains('@'), "no addresses in the digest");
        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
