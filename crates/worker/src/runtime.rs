//! Runtime plumbing shared by the scheduler and bounded one-shot jobs.

use anyhow::{Context, Result};
use mazimi_core::{connect_database, DatabaseConfig, PoolRole};
use sqlx::{pool::PoolConnection, PgPool, Postgres};
use std::path::Path;
use tracing_subscriber::EnvFilter;

const SCHEDULER_LOCK_KEY: i64 = 0x4d41_5a49_4d49; // ASCII: MAZIMI
pub const WORKER_HEARTBEAT_PATH: &str = "/tmp/mazimi-worker.ready";

pub async fn connect_worker_pool() -> Result<PgPool> {
    let config = DatabaseConfig::from_env(PoolRole::Worker)?;
    connect_database(&config).await
}

pub async fn connect_job_pool() -> Result<PgPool> {
    let config = DatabaseConfig::from_env(PoolRole::OneShot)?;
    connect_database(&config).await
}

pub fn environment() -> Result<String> {
    let value = std::env::var("APP_ENV")
        .unwrap_or_else(|_| "production".to_string())
        .trim()
        .to_ascii_lowercase();
    anyhow::ensure!(
        matches!(value.as_str(), "local" | "test" | "staging" | "production"),
        "APP_ENV must be one of: local, test, staging, production"
    );
    Ok(value)
}

pub fn init_tracing(app_env: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if matches!(app_env, "staging" | "production") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

/// A session-level PostgreSQL advisory lock prevents duplicate schedulers
/// during rolling deploys or accidental horizontal scaling. One-shot jobs are
/// intentionally independent and rely on their bounded/idempotent contracts.
pub struct SchedulerLease {
    connection: PoolConnection<Postgres>,
}

impl SchedulerLease {
    pub async fn acquire(pool: &PgPool) -> Result<Self> {
        let mut connection = pool
            .acquire()
            .await
            .context("failed to reserve scheduler lock connection")?;
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(SCHEDULER_LOCK_KEY)
            .fetch_one(&mut *connection)
            .await
            .context("failed to acquire scheduler advisory lock")?;
        anyhow::ensure!(
            acquired,
            "another Mazimi worker scheduler is already active"
        );
        Ok(Self { connection })
    }

    pub async fn release(mut self) {
        if let Err(error) = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(SCHEDULER_LOCK_KEY)
            .fetch_one(&mut *self.connection)
            .await
        {
            tracing::warn!(error = %error, "failed to explicitly release scheduler lock");
        }
        if let Err(error) = self.connection.close().await {
            tracing::warn!(error = %error, "failed to close scheduler lock connection");
        }
    }
}

pub async fn write_worker_heartbeat() -> Result<()> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    tokio::fs::write(WORKER_HEARTBEAT_PATH, timestamp)
        .await
        .context("failed to update worker heartbeat")
}

pub async fn remove_worker_heartbeat() {
    if Path::new(WORKER_HEARTBEAT_PATH).exists() {
        if let Err(error) = tokio::fs::remove_file(WORKER_HEARTBEAT_PATH).await {
            tracing::warn!(error = %error, "failed to remove worker heartbeat");
        }
    }
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to register Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                () = ctrl_c => {},
                _ = terminate.recv() => {},
            },
            Err(error) => {
                tracing::error!(error = %error, "failed to register SIGTERM handler");
                ctrl_c.await;
            }
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
