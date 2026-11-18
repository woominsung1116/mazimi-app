//! 마감 D-7/D-3/D-1 알림 생성 + 발송 워커.
//!
//! 동작 원칙:
//! - alert_subscriptions(enabled=true)가 있는 유저 × 해당 마감 프로그램 조합만 처리
//! - (user_id, program_id, alert_type, alert_date) UNIQUE → 중복 발송 자동 방지
//! - 하루 2회 제한: 오늘 이미 2건 이상 발송된 유저는 skip (D-1은 마지막 기회라 예외)
//! - 발송 시간대: 09:00~20:59 KST 밖에서는 생성·발송 모두 다음 시간으로 미룸
//! - 알림 채널(카카오/FCM) 실패는 워크플로를 중단하지 않음

use anyhow::Result;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Timelike, Utc};
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::notifications::{NotificationDispatcher, NotificationPayload};

/// 하루 최대 알림 발송 횟수
const DAILY_ALERT_LIMIT: i64 = 2;

/// KST 발송 허용 시간대 (시 단위, 양끝 포함). 자정 직후의 일괄 발송은 사용자가
/// 구독한 정보라도 야간 광고성 전송 제한(정보통신망법 §50③, 21~08시)과 충돌하므로
/// 09:00~20:59 KST에만 생성·발송한다. 매시간 cron은 이 창 밖에서는 아무 일도 하지 않는다.
const DELIVERY_WINDOW_HOURS_KST: std::ops::RangeInclusive<u32> = 9..=20;

/// deadline_at / application_end_at 기준으로 D-7, D-3, D-1 알림을 생성·발송한다.
/// 매시간 호출되지만 UNIQUE 제약으로 같은 날 중복 실행 시 안전하다.
pub async fn generate_deadline_alerts(
    pool: &PgPool,
    notifier: &NotificationDispatcher,
) -> Result<()> {
    let now = Utc::now();
    if !within_delivery_window(now) {
        tracing::debug!("outside the KST delivery window; deadline alerts deferred");
        return Ok(());
    }
    let today = seoul_date(now);

    for days_left in [1i64, 3, 7] {
        let target_date = today + Duration::days(days_left);
        let alert_type = format!("d{}", days_left);

        run_for_type(pool, notifier, today, target_date, &alert_type, days_left).await?;
    }

    Ok(())
}

/// alert_subscriptions 조회 결과 행.
#[derive(Debug, FromRow)]
struct AlertCandidate {
    user_id: Uuid,
    program_id: Uuid,
    /// users.phone (없으면 None)
    phone: Option<String>,
    /// programs.title
    title: String,
    expo_tokens: Vec<String>,
    in_app_visible: bool,
    /// programs.benefit_amount_monthly (원 단위 정수)
    benefit_amount_monthly: Option<i32>,
    /// programs.deadline_at (우선) 또는 application_end_at
    effective_deadline: Option<DateTime<Utc>>,
}

const DEADLINE_CANDIDATES_SQL: &str = r#"
        SELECT
            asub.user_id,
            asub.program_id,
            CASE WHEN COALESCE(pref.channel_kakao, false) THEN u.phone ELSE NULL END AS phone,
            CASE WHEN COALESCE(pref.channel_push, true) THEN ARRAY(
                SELECT pt.token FROM push_tokens pt WHERE pt.user_id=u.id
                ORDER BY pt.updated_at DESC
            ) ELSE ARRAY[]::text[] END AS expo_tokens,
            COALESCE(pref.channel_in_app, true) AS in_app_visible,
            p.title,
            -- A corporate offer's amount is no support money: never in a push.
            CASE WHEN p.program_type = 'corporate_benefit' THEN NULL
                 ELSE p.benefit_amount_monthly END AS benefit_amount_monthly,
            LEAST(p.deadline_at, p.application_end_at, p.benefit_valid_until) AS effective_deadline
        FROM alert_subscriptions asub
        JOIN current_programs p ON p.id = asub.program_id
        JOIN users    u ON u.id = asub.user_id
        LEFT JOIN notification_preferences pref ON pref.user_id = u.id
        WHERE asub.enabled = true
          AND COALESCE(pref.notify_deadline, true)
          AND (COALESCE(pref.channel_in_app, true) OR COALESCE(pref.channel_push, true) OR COALESCE(pref.channel_kakao, false))
          AND NOT EXISTS (SELECT 1 FROM user_program_states ups WHERE ups.user_id=u.id AND ups.program_id=p.id AND ups.state IN ('applied','waiting','received','abandoned'))
          -- The KST day of the last open second: an end at 24:00 KST is
          -- that day's deadline (the list's seoul_date(deadline - 1s)).
          AND DATE(
                (LEAST(p.deadline_at, p.application_end_at, p.benefit_valid_until) - interval '1 second')
                    AT TIME ZONE 'Asia/Seoul'
              ) = $1
        "#;

/// 단일 alert_type(d7/d3/d1)에 대한 알림 생성·발송 처리.
async fn run_for_type(
    pool: &PgPool,
    notifier: &NotificationDispatcher,
    today: chrono::NaiveDate,
    target_date: chrono::NaiveDate,
    alert_type: &str,
    days_left: i64,
) -> Result<()> {
    // alert_subscriptions(enabled=true) × 마감일이 target_date인 프로그램 × 유저 정보
    let candidates: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
        .bind(target_date)
        .fetch_all(pool)
        .await?;

    if candidates.is_empty() {
        tracing::debug!(alert_type, "no candidates for deadline alert");
        return Ok(());
    }

    tracing::info!(
        alert_type,
        candidate_count = candidates.len(),
        "processing deadline alert candidates"
    );

    let mut queued = 0u32;
    let mut skipped_limit = 0u32;
    let mut skipped_dup = 0u32;

    for candidate in candidates {
        // Serialize the count and insertion per user, including overlapping jobs.
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(candidate.user_id)
            .execute(&mut *tx)
            .await?;
        // ── 하루 2회 제한 체크 ──
        let today_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM alert_deliveries WHERE user_id = $1 AND alert_date = $2",
        )
        .bind(candidate.user_id)
        .bind(today)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            tracing::warn!(
                user_id = %candidate.user_id,
                error = %e,
                "Failed to check daily alert count"
            );
            e
        })?;

        if daily_limit_reached(today_count, days_left) {
            skipped_limit += 1;
            tracing::debug!(
                user_id     = %candidate.user_id,
                today_count,
                "daily alert limit reached, skipping"
            );
            continue;
        }

        // ── INSERT … ON CONFLICT DO NOTHING → 중복 발송 방지 ──
        let result = sqlx::query(
            "INSERT INTO alert_deliveries \
                 (id, user_id, program_id, alert_type, alert_date, in_app_visible) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (user_id, program_id, alert_type, alert_date) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(candidate.user_id)
        .bind(candidate.program_id)
        .bind(alert_type)
        .bind(today)
        .bind(candidate.in_app_visible)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        if result.rows_affected() == 0 {
            skipped_dup += 1;
            continue;
        }

        queued += 1;
        tracing::info!(
            user_id    = %candidate.user_id,
            program_id = %candidate.program_id,
            alert_type,
            "deadline alert queued"
        );

        // ── 알림 발송 ──
        let payload = build_payload(&candidate, days_left);
        if let Err(e) = notifier.send(&payload).await {
            // 발송 실패는 warn만 남기고 계속 (DB 기록은 이미 완료)
            tracing::warn!(
                user_id    = %candidate.user_id,
                program_id = %candidate.program_id,
                alert_type,
                error = %e,
                "notification dispatch failed (alert already recorded in DB)"
            );
        }
    }

    tracing::info!(
        alert_type,
        queued,
        skipped_limit,
        skipped_dup,
        "deadline alert run complete"
    );

    Ok(())
}

/// `AlertCandidate`로부터 `NotificationPayload`를 조립한다.
/// 카카오 템플릿 변수와 FCM 본문을 동시에 구성한다.
fn build_payload(c: &AlertCandidate, days_left: i64) -> NotificationPayload {
    // 카카오 템플릿 코드: DEADLINE_D7 / DEADLINE_D3 / DEADLINE_D1
    let template_code = format!("DEADLINE_D{}", days_left);

    // 마감 표현 문자열 (예: "7일 후", "내일")
    let deadline_label = match days_left {
        1 => "내일".to_string(),
        n => format!("{}일 후", n),
    };

    // 혜택 금액 문자열 (없으면 빈 문자열)
    let benefit_str = c
        .benefit_amount_monthly
        .filter(|amt| *amt > 0)
        .map(|amt| {
            if amt % 10_000 == 0 {
                format!("월 {}만원", amt / 10_000)
            } else {
                format!("월 {}원", amt)
            }
        })
        .unwrap_or_default();

    // 알림 본문: 행동경제학 손실 프레이밍 적용
    let body = if benefit_str.is_empty() {
        format!(
            "{} 마감! '{}'에 지원하지 않으면 혜택을 놓칠 수 있어요.",
            deadline_label, c.title
        )
    } else {
        format!(
            "{} 마감! '{}' {} 혜택이 {} 사라질 수 있어요.",
            deadline_label, c.title, benefit_str, deadline_label
        )
    };

    // 카카오 템플릿 변수 치환 맵
    let mut template_vars = HashMap::new();
    template_vars.insert("#{프로그램명}".to_string(), c.title.clone());
    template_vars.insert("#{마감시점}".to_string(), deadline_label.clone());
    if !benefit_str.is_empty() {
        template_vars.insert("#{혜택금액}".to_string(), benefit_str);
    }
    if let Some(deadline) = c.effective_deadline {
        template_vars.insert(
            "#{마감일}".to_string(),
            // The same last-open-second day the candidate query matched.
            (deadline - Duration::seconds(1))
                .with_timezone(&seoul_offset())
                .format("%m월 %d일")
                .to_string(),
        );
    }

    NotificationPayload {
        user_id: c.user_id,
        phone: c.phone.clone(),
        expo_tokens: c.expo_tokens.clone(),
        program_id: Some(c.program_id),
        template_code,
        title: format!("마지미 마감 알림 ({})", deadline_label),
        body,
        template_vars,
    }
}

fn seoul_offset() -> FixedOffset {
    FixedOffset::east_opt(9 * 60 * 60).expect("valid KST offset")
}

fn seoul_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&seoul_offset()).date_naive()
}

/// True between 09:00 and 20:59 Korean time, the only hours alerts may leave.
fn within_delivery_window(now: DateTime<Utc>) -> bool {
    DELIVERY_WINDOW_HOURS_KST.contains(&now.with_timezone(&seoul_offset()).hour())
}

/// D-1 is the last chance to act, so it bypasses the daily cap; D-3/D-7 wait.
fn daily_limit_reached(today_count: i64, days_left: i64) -> bool {
    days_left != 1 && today_count >= DAILY_ALERT_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_day_changes_at_korean_midnight() {
        let before = "2026-10-01T14:59:59Z".parse::<DateTime<Utc>>().unwrap();
        let after = "2026-10-01T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(seoul_date(before).to_string(), "2026-10-01");
        assert_eq!(seoul_date(after).to_string(), "2026-10-02");
    }

    #[test]
    fn alerts_leave_only_between_nine_and_twenty_one_korean_time() {
        let at = |utc: &str| within_delivery_window(utc.parse::<DateTime<Utc>>().unwrap());
        assert!(!at("2026-10-01T15:00:00Z"), "00:00 KST");
        assert!(!at("2026-10-01T23:59:59Z"), "08:59 KST");
        assert!(at("2026-10-02T00:00:00Z"), "09:00 KST");
        assert!(at("2026-10-02T03:30:00Z"), "12:30 KST");
        assert!(at("2026-10-02T11:59:59Z"), "20:59 KST");
        assert!(!at("2026-10-02T12:00:00Z"), "21:00 KST");
    }

    #[test]
    fn last_chance_alert_is_exempt_from_the_daily_cap() {
        assert!(daily_limit_reached(DAILY_ALERT_LIMIT, 3));
        assert!(daily_limit_reached(DAILY_ALERT_LIMIT, 7));
        assert!(!daily_limit_reached(DAILY_ALERT_LIMIT - 1, 3));
        assert!(!daily_limit_reached(DAILY_ALERT_LIMIT, 1));
        assert!(!daily_limit_reached(DAILY_ALERT_LIMIT + 5, 1));
    }

    #[test]
    fn deadline_payload_reaches_devices_and_opens_its_program() {
        let candidate = AlertCandidate {
            user_id: Uuid::new_v4(),
            program_id: Uuid::new_v4(),
            phone: None,
            in_app_visible: true,
            expo_tokens: vec![
                "ExpoPushToken[fixture-one]".into(),
                "ExpoPushToken[fixture-two]".into(),
            ],
            title: "마감 알림 테스트".into(),
            benefit_amount_monthly: Some(300_000),
            effective_deadline: Some("2026-10-01T16:00:00Z".parse().unwrap()),
        };
        let payload = build_payload(&candidate, 1);
        assert_eq!(payload.program_id, Some(candidate.program_id));
        assert_eq!(payload.expo_tokens, candidate.expo_tokens);
        assert_eq!(payload.phone, None);
        assert_eq!(payload.template_vars["#{마감일}"], "10월 02일");
        assert!(payload.body.contains("월 30만원"));
    }
    /// The alert day is the KST day of the last open second: an end at
    /// 24:00 KST (00:00 of the next day) is that day's deadline, as on the
    /// list (`seoul_date(deadline - 1s)`); 23:59:59 KST stays the same day.
    #[tokio::test]
    async fn the_alert_day_is_the_last_open_kst_day() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database is required");
        let mut tx = pool.begin().await.unwrap();
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id) VALUES($1)")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let target = seoul_date(Utc::now()) + Duration::days(3);
        let kst = |day: NaiveDate, h: u32, m: u32, s: u32| {
            day.and_hms_opt(h, m, s)
                .unwrap()
                .and_local_timezone(seoul_offset())
                .unwrap()
                .with_timezone(&Utc)
        };
        let next = target + Duration::days(1);
        for (deadline, expected) in [
            (kst(next, 0, 0, 0), true),
            (kst(target, 23, 59, 59), true),
            (kst(next, 0, 0, 1), false),
        ] {
            let program_id = Uuid::new_v4();
            sqlx::query("INSERT INTO programs(id,program_type,source_type,title,deadline_at) VALUES($1,'welfare','manual','알림 날짜 검증',$2)")
                .bind(program_id).bind(deadline).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO alert_subscriptions(user_id,program_id) VALUES($1,$2)")
                .bind(user_id)
                .bind(program_id)
                .execute(&mut *tx)
                .await
                .unwrap();
            let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
                .bind(target)
                .fetch_all(&mut *tx)
                .await
                .unwrap();
            assert_eq!(
                rows.iter().any(|r| r.program_id == program_id),
                expected,
                "{deadline}"
            );
        }
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn deadline_candidates_respect_preferences_and_catalogue_visibility() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database is required");
        let mut tx = pool.begin().await.unwrap();
        let user_id = Uuid::new_v4();
        let program_id = Uuid::new_v4();
        let target = seoul_date(Utc::now()) + Duration::days(3);
        let deadline = target
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_local_timezone(seoul_offset())
            .unwrap()
            .with_timezone(&Utc);
        sqlx::query("INSERT INTO users(id,phone) VALUES($1,'01000000000')")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,deadline_at) VALUES($1,'welfare','manual','알림 회귀 테스트',$2)")
            .bind(program_id).bind(deadline).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO alert_subscriptions(user_id,program_id) VALUES($1,$2)")
            .bind(user_id)
            .bind(program_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let tokens = vec![
            format!("ExpoPushToken[{}]", Uuid::new_v4().simple()),
            format!("ExpoPushToken[{}]", Uuid::new_v4().simple()),
        ];
        for token in &tokens {
            sqlx::query("INSERT INTO push_tokens(user_id,token,platform) VALUES($1,$2,'android')")
                .bind(user_id)
                .bind(token)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        // Missing preferences follows schema defaults: push enabled, Kakao opt-in.
        let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
            .bind(target)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let row = rows.iter().find(|r| r.user_id == user_id).unwrap();
        assert_eq!(row.expo_tokens.len(), 2);
        assert!(tokens.iter().all(|token| row.expo_tokens.contains(token)));
        assert!(row.phone.is_none());
        assert!(row.in_app_visible);
        // A corporate offer's amount never reaches the push text.
        sqlx::query("UPDATE programs SET benefit_amount_monthly=50000 WHERE id=$1")
            .bind(program_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        for (program_type, amount) in [("welfare", Some(50_000)), ("corporate_benefit", None)] {
            sqlx::query("UPDATE programs SET program_type=$2 WHERE id=$1")
                .bind(program_id)
                .bind(program_type)
                .execute(&mut *tx)
                .await
                .unwrap();
            let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
                .bind(target)
                .fetch_all(&mut *tx)
                .await
                .unwrap();
            let row = rows.iter().find(|r| r.user_id == user_id).unwrap();
            assert_eq!(row.benefit_amount_monthly, amount, "{program_type}");
        }
        sqlx::query(
            "UPDATE programs SET program_type='welfare', benefit_amount_monthly=NULL WHERE id=$1",
        )
        .bind(program_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO notification_preferences(user_id,channel_push,channel_kakao,channel_in_app) VALUES($1,false,true,false)")
            .bind(user_id).execute(&mut *tx).await.unwrap();
        let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
            .bind(target)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        let row = rows.iter().find(|r| r.user_id == user_id).unwrap();
        assert!(row.expo_tokens.is_empty());
        assert!(!row.in_app_visible);
        assert_eq!(row.phone.as_deref(), Some("01000000000"));
        sqlx::query("UPDATE notification_preferences SET notify_deadline=false WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
            .bind(target)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert!(!rows.iter().any(|r| r.user_id == user_id));
        sqlx::query("UPDATE notification_preferences SET notify_deadline=true WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        for status in ["review_required", "closed"] {
            sqlx::query("UPDATE programs SET program_status=$2 WHERE id=$1")
                .bind(program_id)
                .bind(status)
                .execute(&mut *tx)
                .await
                .unwrap();
            let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
                .bind(target)
                .fetch_all(&mut *tx)
                .await
                .unwrap();
            assert!(!rows.iter().any(|r| r.user_id == user_id));
        }
        sqlx::query("UPDATE programs SET program_status='active', deadline_at=now()-interval '1 day' WHERE id=$1")
            .bind(program_id).execute(&mut *tx).await.unwrap();
        let rows: Vec<AlertCandidate> = sqlx::query_as(DEADLINE_CANDIDATES_SQL)
            .bind(target)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert!(!rows.iter().any(|r| r.user_id == user_id));
        tx.rollback().await.unwrap();
        pool.close().await;
    }
}
