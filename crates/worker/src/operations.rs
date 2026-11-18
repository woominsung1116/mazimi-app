//! Durable operator incidents and optional webhook delivery.
//!
//! An event fingerprint represents one incident lifecycle. Repeated failures
//! increment the same row instead of flooding the admin queue. Once resolved,
//! the same fingerprint can reopen and becomes eligible for delivery again.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

const MAX_DELIVERY_ATTEMPTS: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }

    fn rank(self) -> i32 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
            Self::Critical => 3,
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            "critical" => Ok(Self::Critical),
            _ => anyhow::bail!("OPS_ALERT_MIN_SEVERITY must be info, warning, error, or critical"),
        }
    }
}

#[derive(Debug)]
pub struct EventCommand<'a> {
    pub fingerprint: &'a str,
    pub severity: Severity,
    pub component: &'a str,
    pub event_type: &'a str,
    pub subject_type: Option<&'a str>,
    pub subject_id: Option<&'a str>,
    /// Operator-safe summary. Upstream response bodies and secrets must never
    /// be placed here or in `details`.
    pub message: &'a str,
    pub details: Value,
}

pub async fn record_event(pool: &PgPool, command: &EventCommand<'_>) -> Result<Uuid> {
    validate_event(command)?;
    let mut tx = pool.begin().await?;
    let id = record_event_tx(&mut tx, command).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn record_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    command: &EventCommand<'_>,
) -> Result<Uuid> {
    validate_event(command)?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO operational_events (
             fingerprint, severity, component, event_type, subject_type,
             subject_id, message, details
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (fingerprint) DO UPDATE SET
             severity = EXCLUDED.severity,
             component = EXCLUDED.component,
             event_type = EXCLUDED.event_type,
             subject_type = EXCLUDED.subject_type,
             subject_id = EXCLUDED.subject_id,
             message = EXCLUDED.message,
             details = EXCLUDED.details,
             status = 'open',
             occurrence_count = LEAST(operational_events.occurrence_count + 1, 100000000),
             last_occurred_at = now(),
             acknowledged_at = NULL,
             acknowledged_by = NULL,
             resolved_at = NULL,
             resolved_by = NULL,
             resolution_note = NULL,
             next_delivery_at = CASE
                 WHEN operational_events.status = 'resolved' THEN now()
                 ELSE operational_events.next_delivery_at
             END,
             delivery_attempt_count = CASE
                 WHEN operational_events.status = 'resolved' THEN 0
                 ELSE operational_events.delivery_attempt_count
             END,
             delivered_at = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.delivered_at
             END,
             delivery_claim_token = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.delivery_claim_token
             END,
             delivery_claimed_until = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.delivery_claimed_until
             END,
             last_delivery_error = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.last_delivery_error
             END,
             email_delivered_at = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.email_delivered_at
             END,
             email_delivery_attempt_count = CASE
                 WHEN operational_events.status = 'resolved' THEN 0
                 ELSE operational_events.email_delivery_attempt_count
             END,
             email_next_delivery_at = CASE
                 WHEN operational_events.status = 'resolved' THEN now()
                 ELSE operational_events.email_next_delivery_at
             END,
             email_claimed_until = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.email_claimed_until
             END,
             email_last_delivery_error = CASE
                 WHEN operational_events.status = 'resolved' THEN NULL
                 ELSE operational_events.email_last_delivery_error
             END,
             updated_at = now()
         RETURNING id",
    )
    .bind(command.fingerprint)
    .bind(command.severity.as_str())
    .bind(command.component)
    .bind(command.event_type)
    .bind(command.subject_type)
    .bind(command.subject_id)
    .bind(command.message)
    .bind(&command.details)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

pub async fn resolve_event(
    pool: &PgPool,
    fingerprint: &str,
    resolved_by: Option<Uuid>,
    note: Option<&str>,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let resolved = resolve_event_tx(&mut tx, fingerprint, resolved_by, note).await?;
    tx.commit().await?;
    Ok(resolved)
}

pub async fn resolve_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    fingerprint: &str,
    resolved_by: Option<Uuid>,
    note: Option<&str>,
) -> Result<bool> {
    anyhow::ensure!(
        !fingerprint.is_empty() && fingerprint.len() <= 200,
        "invalid fingerprint"
    );
    anyhow::ensure!(
        note.is_none_or(|note| note.chars().count() <= 1_000),
        "resolution note too long"
    );
    let affected = sqlx::query(
        "UPDATE operational_events
         SET status = 'resolved', resolved_at = now(), resolved_by = $2,
             resolution_note = $3, delivery_claim_token = NULL,
             delivery_claimed_until = NULL, updated_at = now()
         WHERE fingerprint = $1 AND status <> 'resolved'",
    )
    .bind(fingerprint)
    .bind(resolved_by)
    .bind(note)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

fn validate_event(command: &EventCommand<'_>) -> Result<()> {
    validate_len("fingerprint", command.fingerprint, 200)?;
    validate_len("component", command.component, 100)?;
    validate_len("event_type", command.event_type, 100)?;
    validate_len("message", command.message, 500)?;
    if let Some(value) = command.subject_type {
        validate_len("subject_type", value, 100)?;
    }
    if let Some(value) = command.subject_id {
        validate_len("subject_id", value, 200)?;
    }
    anyhow::ensure!(
        command.details.is_object(),
        "event details must be a JSON object"
    );
    Ok(())
}

fn validate_len(field: &str, value: &str, max: usize) -> Result<()> {
    let count = value.chars().count();
    anyhow::ensure!(
        (1..=max).contains(&count),
        "{field} must contain 1 to {max} characters"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum WebhookFormat {
    Generic,
    Slack,
    Discord,
}

impl WebhookFormat {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "generic" => Ok(Self::Generic),
            "slack" => Ok(Self::Slack),
            "discord" => Ok(Self::Discord),
            _ => anyhow::bail!("OPS_ALERT_WEBHOOK_FORMAT must be generic, slack, or discord"),
        }
    }
}

#[derive(Debug)]
struct DeliveryConfig {
    url: reqwest::Url,
    format: WebhookFormat,
    minimum_severity: Severity,
    max_batch: i64,
    dashboard_url: Option<String>,
}

impl DeliveryConfig {
    fn from_env() -> Result<Option<Self>> {
        let url = match std::env::var("OPS_ALERT_WEBHOOK_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(None),
        };
        let url = reqwest::Url::parse(url.trim()).context("invalid OPS_ALERT_WEBHOOK_URL")?;
        anyhow::ensure!(
            url.scheme() == "https" && url.username().is_empty() && url.password().is_none(),
            "OPS_ALERT_WEBHOOK_URL must be HTTPS and must not use URL credentials"
        );
        let format = WebhookFormat::parse(
            &std::env::var("OPS_ALERT_WEBHOOK_FORMAT").unwrap_or_else(|_| "generic".to_string()),
        )?;
        let minimum_severity = Severity::parse(
            &std::env::var("OPS_ALERT_MIN_SEVERITY").unwrap_or_else(|_| "warning".to_string()),
        )?;
        let max_batch = parse_bounded_i64_env("OPS_ALERT_MAX_BATCH", 20, 1, 100)?;
        let dashboard_url = std::env::var("OPS_ALERT_DASHBOARD_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Ok(Some(Self {
            url,
            format,
            minimum_severity,
            max_batch,
            dashboard_url,
        }))
    }
}

#[derive(Debug, FromRow, Serialize)]
struct DeliveryEvent {
    id: Uuid,
    severity: String,
    component: String,
    event_type: String,
    subject_type: Option<String>,
    subject_id: Option<String>,
    message: String,
    details: Value,
    occurrence_count: i32,
    last_occurred_at: DateTime<Utc>,
    delivery_attempt_count: i32,
}

#[derive(Debug, Default, Serialize)]
pub struct DeliverySummary {
    pub selected: usize,
    pub delivered: usize,
    pub failed: usize,
}

pub async fn dispatch_pending_from_env(pool: &PgPool) -> Result<Option<DeliverySummary>> {
    let Some(config) = DeliveryConfig::from_env()? else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mazimi-Ops-Notifier/1.0")
        .build()?;
    dispatch_pending(pool, &client, &config).await.map(Some)
}

async fn dispatch_pending(
    pool: &PgPool,
    client: &reqwest::Client,
    config: &DeliveryConfig,
) -> Result<DeliverySummary> {
    let claim_token = Uuid::new_v4();
    let events = sqlx::query_as::<_, DeliveryEvent>(
        "WITH selected AS (
             SELECT id
             FROM operational_events
             WHERE status <> 'resolved'
               AND (delivered_at IS NULL OR (status = 'open' AND delivered_at < now() - interval '1 day'
                    AND last_occurred_at > delivered_at))
               AND next_delivery_at <= now()
               AND (delivery_claimed_until IS NULL OR delivery_claimed_until < now())
               AND CASE severity
                   WHEN 'info' THEN 0 WHEN 'warning' THEN 1
                   WHEN 'error' THEN 2 WHEN 'critical' THEN 3
                   ELSE -1
               END >= $1
             ORDER BY
               CASE severity
                   WHEN 'critical' THEN 3 WHEN 'error' THEN 2
                   WHEN 'warning' THEN 1 ELSE 0
               END DESC,
               last_occurred_at
             FOR UPDATE SKIP LOCKED
             LIMIT $2
         )
         UPDATE operational_events event
         SET delivery_claim_token = $3,
             delivery_claimed_until = now() + interval '5 minutes',
             updated_at = now()
         FROM selected
         WHERE event.id = selected.id
         RETURNING event.id, event.severity, event.component, event.event_type,
                   event.subject_type, event.subject_id, event.message,
                   event.details, event.occurrence_count, event.last_occurred_at,
                   event.delivery_attempt_count",
    )
    .bind(config.minimum_severity.rank())
    .bind(config.max_batch)
    .bind(claim_token)
    .fetch_all(pool)
    .await?;

    let mut summary = DeliverySummary {
        selected: events.len(),
        ..DeliverySummary::default()
    };
    for event in events {
        let payload = webhook_payload(&event, config);
        let delivery_error = match client.post(config.url.clone()).json(&payload).send().await {
            Ok(response) if response.status().is_success() => None,
            Ok(response) => Some(format!(
                "webhook returned HTTP {}",
                response.status().as_u16()
            )),
            Err(_) => Some("webhook request failed".to_string()),
        };

        if let Some(error) = delivery_error {
            summary.failed += 1;
            let attempt = (event.delivery_attempt_count + 1).min(MAX_DELIVERY_ATTEMPTS);
            let retry_seconds = retry_delay_seconds(attempt);
            sqlx::query(
                "UPDATE operational_events
                 SET delivery_attempt_count = LEAST(delivery_attempt_count + 1, 20),
                     next_delivery_at = now() + make_interval(secs => $3),
                     last_delivery_error = $4,
                     delivery_claim_token = NULL, delivery_claimed_until = NULL,
                     updated_at = now()
                 WHERE id = $1 AND delivery_claim_token = $2",
            )
            .bind(event.id)
            .bind(claim_token)
            .bind(retry_seconds as f64)
            .bind(error)
            .execute(pool)
            .await?;
        } else {
            summary.delivered += 1;
            sqlx::query(
                "UPDATE operational_events
                 SET delivered_at = now(), last_delivery_error = NULL,
                     delivery_attempt_count = 0,
                     delivery_claim_token = NULL, delivery_claimed_until = NULL,
                     updated_at = now()
                 WHERE id = $1 AND delivery_claim_token = $2",
            )
            .bind(event.id)
            .bind(claim_token)
            .execute(pool)
            .await?;
        }
    }
    Ok(summary)
}

fn webhook_payload(event: &DeliveryEvent, config: &DeliveryConfig) -> Value {
    let dashboard = config
        .dashboard_url
        .as_deref()
        .map(|url| format!("\n{url}"))
        .unwrap_or_default();
    let text = format!(
        "[Mazimi][{}][{}] {} ({}회){}",
        event.severity.to_ascii_uppercase(),
        event.component,
        event.message,
        event.occurrence_count,
        dashboard
    );
    match config.format {
        WebhookFormat::Slack => json!({ "text": text }),
        WebhookFormat::Discord => json!({ "content": text }),
        WebhookFormat::Generic => json!({
            "text": text,
            "event": {
                "id": event.id,
                "severity": event.severity,
                "component": event.component,
                "event_type": event.event_type,
                "subject_type": event.subject_type,
                "subject_id": event.subject_id,
                "message": event.message,
                "details": event.details,
                "occurrence_count": event.occurrence_count,
                "last_occurred_at": event.last_occurred_at
            }
        }),
    }
}

/// Immediate email for open incidents at or above
/// `OPS_ALERT_EMAIL_MIN_SEVERITY` (default `error`), batched into one message
/// per pass. Deduplicated per incident: one email per lifecycle, plus at most
/// one reminder a day while it keeps recurring unacknowledged. A failed send
/// backs off per incident (1 minute doubling to 6 hours) and never blocks
/// other jobs.
pub async fn dispatch_email_pending_from_env(pool: &PgPool) -> Result<Option<DeliverySummary>> {
    let Some(config) = crate::ops_email::EmailConfig::from_env()? else {
        return Ok(None);
    };
    let minimum = Severity::parse(
        &std::env::var("OPS_ALERT_EMAIL_MIN_SEVERITY").unwrap_or_else(|_| "error".to_string()),
    )
    .context("OPS_ALERT_EMAIL_MIN_SEVERITY must be info, warning, error, or critical")?;
    let max_batch = parse_bounded_i64_env("OPS_ALERT_EMAIL_MAX_BATCH", 10, 1, 50)?;
    let dashboard = std::env::var("OPS_ALERT_DASHBOARD_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    dispatch_email_pending(pool, minimum, max_batch, |subject, text| {
        let config = config.clone();
        let text = match &dashboard {
            Some(url) => format!("{text}\n관리 화면: {}", url.trim()),
            None => text,
        };
        async move {
            let html = crate::ops_email::html_from_text(&subject, &text);
            config.send(&subject, &text, &html).await
        }
    })
    .await
    .map(Some)
}

pub async fn dispatch_email_pending<F, Fut>(
    pool: &PgPool,
    minimum: Severity,
    max_batch: i64,
    send: F,
) -> Result<DeliverySummary>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let events = sqlx::query_as::<_, DeliveryEvent>(
        "WITH selected AS (
             SELECT id FROM operational_events
             WHERE status <> 'resolved'
               AND (email_delivered_at IS NULL OR (
                    status = 'open' AND email_delivered_at < now() - interval '1 day'
                    AND last_occurred_at > email_delivered_at))
               AND email_next_delivery_at <= now()
               AND (email_claimed_until IS NULL OR email_claimed_until < now())
               AND CASE severity
                   WHEN 'info' THEN 0 WHEN 'warning' THEN 1
                   WHEN 'error' THEN 2 WHEN 'critical' THEN 3 ELSE -1
               END >= $1
             ORDER BY CASE severity WHEN 'critical' THEN 3 WHEN 'error' THEN 2
                      WHEN 'warning' THEN 1 ELSE 0 END DESC, last_occurred_at
             FOR UPDATE SKIP LOCKED
             LIMIT $2
         )
         UPDATE operational_events event
         SET email_claimed_until = now() + interval '5 minutes', updated_at = now()
         FROM selected WHERE event.id = selected.id
         RETURNING event.id, event.severity, event.component, event.event_type,
                   event.subject_type, event.subject_id, event.message,
                   event.details, event.occurrence_count, event.last_occurred_at,
                   event.email_delivery_attempt_count AS delivery_attempt_count",
    )
    .bind(minimum.rank())
    .bind(max_batch)
    .fetch_all(pool)
    .await?;
    let mut summary = DeliverySummary {
        selected: events.len(),
        ..DeliverySummary::default()
    };
    if events.is_empty() {
        return Ok(summary);
    }
    let ids: Vec<Uuid> = events.iter().map(|event| event.id).collect();
    let worst = events
        .first()
        .map(|event| event.severity.to_ascii_uppercase())
        .unwrap_or_default();
    let subject = format!("[마지미 운영][{worst}] 운영 이벤트 {}건", events.len());
    let mut text = String::from("새로 열렸거나 계속 반복되는 운영 이벤트입니다.\n");
    for event in &events {
        text.push_str(&format!(
            "- [{}][{}] {} ({}회, 최근 {})\n",
            event.severity.to_ascii_uppercase(),
            event.component,
            event.message,
            event.occurrence_count,
            crate::digest::kst(event.last_occurred_at),
        ));
    }
    match send(subject, text).await {
        Ok(()) => {
            summary.delivered = events.len();
            sqlx::query(
                "UPDATE operational_events SET email_delivered_at = now(),
                     email_delivery_attempt_count = 0, email_last_delivery_error = NULL,
                     email_claimed_until = NULL, updated_at = now()
                 WHERE id = ANY($1)",
            )
            .bind(&ids)
            .execute(pool)
            .await?;
        }
        Err(error) => {
            summary.failed = events.len();
            tracing::warn!(error = %error, events = events.len(), "operator alert email failed; retry scheduled");
            for event in &events {
                let attempt = (event.delivery_attempt_count + 1).min(MAX_DELIVERY_ATTEMPTS);
                sqlx::query(
                    "UPDATE operational_events
                     SET email_delivery_attempt_count = LEAST(email_delivery_attempt_count + 1, 20),
                         email_next_delivery_at = now() + make_interval(secs => $2),
                         email_last_delivery_error = 'smtp send failed',
                         email_claimed_until = NULL, updated_at = now()
                     WHERE id = $1",
                )
                .bind(event.id)
                .bind(retry_delay_seconds(attempt) as f64)
                .execute(pool)
                .await?;
            }
        }
    }
    Ok(summary)
}

pub fn webhook_configured() -> bool {
    std::env::var("OPS_ALERT_WEBHOOK_URL").is_ok_and(|value| !value.trim().is_empty())
}

/// Posts plain text to the configured webhook (the digest uses it).
pub async fn post_webhook_text_from_env(text: &str) -> Result<bool> {
    let Some(config) = DeliveryConfig::from_env()? else {
        return Ok(false);
    };
    let payload = match config.format {
        WebhookFormat::Slack | WebhookFormat::Generic => json!({ "text": text }),
        WebhookFormat::Discord => {
            json!({ "content": text.chars().take(1_900).collect::<String>() })
        }
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mazimi-Ops-Notifier/1.0")
        .build()?;
    let response = client
        .post(config.url)
        .json(&payload)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("webhook request failed"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "webhook returned HTTP {}",
        response.status().as_u16()
    );
    Ok(true)
}

fn retry_delay_seconds(attempt: i32) -> i64 {
    let exponent = attempt.saturating_sub(1).min(8) as u32;
    (60_i64.saturating_mul(2_i64.pow(exponent))).min(21_600)
}

fn parse_bounded_i64_env(name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse::<i64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max}"
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_order_is_stable() {
        assert!(Severity::Critical.rank() > Severity::Error.rank());
        assert!(Severity::Error.rank() > Severity::Warning.rank());
        assert!(Severity::Warning.rank() > Severity::Info.rank());
    }

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_delay_seconds(1), 60);
        assert_eq!(retry_delay_seconds(2), 120);
        assert_eq!(retry_delay_seconds(10), 15_360);
        assert_eq!(retry_delay_seconds(100), 15_360);
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required")
    }

    async fn email_pass(pool: &PgPool, fail: bool) -> (DeliverySummary, String) {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let capture = sent.clone();
        let summary =
            dispatch_email_pending(pool, Severity::Error, 50, |subject, text| async move {
                *capture.lock().unwrap() = format!("{subject}\n{text}");
                if fail {
                    anyhow::bail!("smtp send failed (transient)")
                }
                Ok(())
            })
            .await
            .unwrap();
        let text = sent.lock().unwrap().clone();
        (summary, text)
    }

    fn command<'a>(fingerprint: &'a str, severity: Severity, message: &'a str) -> EventCommand<'a> {
        EventCommand {
            fingerprint,
            severity,
            component: "test",
            event_type: "email_test",
            subject_type: None,
            subject_id: None,
            message,
            details: json!({}),
        }
    }

    /// One email per incident lifecycle, one reminder a day while it keeps
    /// recurring, warnings never emailed, failures backed off.
    #[tokio::test]
    async fn error_events_are_emailed_once_with_daily_reminders_and_backoff() {
        let pool = test_pool().await;
        let tag = Uuid::new_v4().simple().to_string();
        let error_fp = format!("email_test:error:{tag}");
        let error_msg = format!("수집 실패 {tag}");
        let warn_fp = format!("email_test:warning:{tag}");
        let warn_msg = format!("경고만 {tag}");
        record_event(&pool, &command(&error_fp, Severity::Error, &error_msg))
            .await
            .unwrap();
        record_event(&pool, &command(&warn_fp, Severity::Warning, &warn_msg))
            .await
            .unwrap();

        // A failing send schedules a retry and is not retried at once.
        let (summary, text) = email_pass(&pool, true).await;
        assert!(text.contains(&error_msg) && summary.failed >= 1);
        let (error, attempts): (Option<String>, i32) = sqlx::query_as(
            "SELECT email_last_delivery_error, email_delivery_attempt_count FROM operational_events WHERE fingerprint=$1",
        )
        .bind(&error_fp)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((error.as_deref(), attempts), (Some("smtp send failed"), 1));
        assert!(!email_pass(&pool, false).await.1.contains(&error_msg));

        sqlx::query(
            "UPDATE operational_events SET email_next_delivery_at=now() WHERE fingerprint=$1",
        )
        .bind(&error_fp)
        .execute(&pool)
        .await
        .unwrap();
        let (_, text) = email_pass(&pool, false).await;
        assert!(text.contains(&error_msg), "{text}");
        assert!(!text.contains(&warn_msg), "warnings are digest-only");

        // Recurring the same day: no second email.
        record_event(&pool, &command(&error_fp, Severity::Error, &error_msg))
            .await
            .unwrap();
        assert!(!email_pass(&pool, false).await.1.contains(&error_msg));
        // Still recurring a day after the last email: one reminder.
        sqlx::query("UPDATE operational_events SET email_delivered_at=now()-interval '2 days' WHERE fingerprint=$1")
            .bind(&error_fp)
            .execute(&pool)
            .await
            .unwrap();
        record_event(&pool, &command(&error_fp, Severity::Error, &error_msg))
            .await
            .unwrap();
        assert!(email_pass(&pool, false).await.1.contains(&error_msg));
        assert!(!email_pass(&pool, false).await.1.contains(&error_msg));

        sqlx::query("DELETE FROM operational_events WHERE fingerprint = ANY($1)")
            .bind(vec![error_fp, warn_fp])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn event_validation_rejects_non_object_details() {
        let command = EventCommand {
            fingerprint: "test:event",
            severity: Severity::Warning,
            component: "test",
            event_type: "test_event",
            subject_type: None,
            subject_id: None,
            message: "test",
            details: json!([]),
        };
        assert!(validate_event(&command).is_err());
    }
}
