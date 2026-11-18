//! Production-safe PostgreSQL pool construction shared by every binary.
//!
//! Keeping this in one place prevents the API, scheduler and one-shot jobs
//! from drifting into different timeout/retry behaviour after deployment.

use anyhow::{Context, Result};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use std::{str::FromStr, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolRole {
    Api,
    Worker,
    OneShot,
    Migration,
}

impl PoolRole {
    fn application_name(self) -> &'static str {
        match self {
            Self::Api => "mazimi-api",
            Self::Worker => "mazimi-worker",
            Self::OneShot => "mazimi-job",
            Self::Migration => "mazimi-migrate",
        }
    }

    fn max_connections_env(self) -> &'static str {
        match self {
            Self::Api => "API_DB_MAX_CONNECTIONS",
            Self::Worker => "WORKER_DB_MAX_CONNECTIONS",
            Self::OneShot => "JOB_DB_MAX_CONNECTIONS",
            Self::Migration => "MIGRATION_DB_MAX_CONNECTIONS",
        }
    }

    fn default_max_connections(self) -> u32 {
        match self {
            Self::Api => 8,
            Self::Worker => 5,
            Self::OneShot => 2,
            Self::Migration => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    database_url: String,
    role: PoolRole,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub connect_timeout: Duration,
    pub connect_attempts: u32,
}

impl DatabaseConfig {
    pub fn from_env(role: PoolRole) -> Result<Self> {
        dotenvy::dotenv().ok();

        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
        validate_database_url(&database_url)?;

        let max_connections = parse_bounded_u32(
            role.max_connections_env(),
            role.default_max_connections(),
            1,
            50,
        )?;
        let min_connections = parse_bounded_u32("DB_MIN_CONNECTIONS", 0, 0, 10)?;
        anyhow::ensure!(
            min_connections <= max_connections,
            "DB_MIN_CONNECTIONS must not exceed {}",
            role.max_connections_env()
        );

        Ok(Self {
            database_url,
            role,
            max_connections,
            min_connections,
            acquire_timeout: Duration::from_secs(parse_bounded_u64(
                "DB_ACQUIRE_TIMEOUT_SECS",
                15,
                1,
                120,
            )?),
            idle_timeout: Duration::from_secs(parse_bounded_u64(
                "DB_IDLE_TIMEOUT_SECS",
                300,
                30,
                3_600,
            )?),
            max_lifetime: Duration::from_secs(parse_bounded_u64(
                "DB_MAX_LIFETIME_SECS",
                1_800,
                60,
                7_200,
            )?),
            connect_timeout: Duration::from_secs(parse_bounded_u64(
                "DB_CONNECT_TIMEOUT_SECS",
                10,
                1,
                60,
            )?),
            connect_attempts: parse_bounded_u32("DB_CONNECT_ATTEMPTS", 6, 1, 20)?,
        })
    }

    pub fn role(&self) -> PoolRole {
        self.role
    }
}

/// Connect with bounded exponential backoff. The URL is never included in an
/// error or log field, so credentials cannot be exposed by startup failures.
pub async fn connect_database(config: &DatabaseConfig) -> Result<PgPool> {
    let connect_options = PgConnectOptions::from_str(&config.database_url)
        .context("DATABASE_URL is not a valid PostgreSQL DSN")?
        .application_name(config.role.application_name());

    for attempt in 1..=config.connect_attempts {
        let pool_options = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(Some(config.idle_timeout))
            .max_lifetime(Some(config.max_lifetime));

        let result = tokio::time::timeout(
            config.connect_timeout,
            pool_options.connect_with(connect_options.clone()),
        )
        .await;

        match result {
            Ok(Ok(pool)) => return Ok(pool),
            Ok(Err(error)) if attempt < config.connect_attempts => {
                tracing::warn!(
                    role = ?config.role,
                    attempt,
                    max_attempts = config.connect_attempts,
                    category = database_error_category(&error),
                    "database connection attempt failed; retrying"
                );
            }
            Err(_) if attempt < config.connect_attempts => {
                tracing::warn!(
                    role = ?config.role,
                    attempt,
                    max_attempts = config.connect_attempts,
                    "database connection attempt timed out; retrying"
                );
            }
            Ok(Err(error)) => {
                anyhow::bail!(
                    "database connection failed after {} attempt(s) ({})",
                    config.connect_attempts,
                    database_error_category(&error)
                );
            }
            Err(_) => {
                anyhow::bail!(
                    "database connection timed out after {} attempt(s)",
                    config.connect_attempts
                );
            }
        }

        let exponent = attempt.saturating_sub(1).min(5);
        let backoff_ms = 250_u64.saturating_mul(1_u64 << exponent).min(8_000);
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
    }

    unreachable!("connect_attempts is validated to be at least one")
}

fn database_error_category(error: &sqlx::Error) -> &'static str {
    match error {
        sqlx::Error::Configuration(_) => "configuration",
        sqlx::Error::Database(_) => "database_rejected_connection",
        sqlx::Error::Io(_) => "network_io",
        sqlx::Error::Tls(_) => "tls",
        sqlx::Error::PoolTimedOut => "pool_timeout",
        sqlx::Error::PoolClosed => "pool_closed",
        _ => "database_unavailable",
    }
}

fn parse_bounded_u32(key: &str, default: u32, min: u32, max: u32) -> Result<u32> {
    let value = match std::env::var(key) {
        Ok(raw) => raw
            .parse::<u32>()
            .with_context(|| format!("{key} must be an integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("failed to read {key}")),
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{key} must be between {min} and {max}"
    );
    Ok(value)
}

fn parse_bounded_u64(key: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let value = match std::env::var(key) {
        Ok(raw) => raw
            .parse::<u64>()
            .with_context(|| format!("{key} must be an integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("failed to read {key}")),
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{key} must be between {min} and {max}"
    );
    Ok(value)
}

/// Fail fast on connection modes that are incompatible with this service.
pub fn validate_database_url(database_url: &str) -> Result<()> {
    let options = PgConnectOptions::from_str(database_url)
        .map_err(|_| anyhow::anyhow!("DATABASE_URL is not a valid PostgreSQL DSN"))?;
    let host = options.get_host().to_ascii_lowercase();
    let is_supabase_host = host.ends_with(".pooler.supabase.com")
        || (host.starts_with("db.") && host.ends_with(".supabase.co"));
    if is_supabase_host && options.get_port() == 6543 {
        anyhow::bail!(
            "DATABASE_URL uses the Supabase transaction pooler on port 6543, which is incompatible with SQLx prepared statements; use the direct or session-pooler connection on port 5432"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_database_url;

    #[test]
    fn accepts_supabase_session_pooler() {
        validate_database_url(
            "postgres://postgres.project:secret@aws-0-ap-northeast-2.pooler.supabase.com:5432/postgres?sslmode=require",
        )
        .unwrap();
    }

    #[test]
    fn rejects_supabase_transaction_pooler_without_leaking_credentials() {
        let error = validate_database_url(
            "postgres://postgres.project:do-not-leak@aws-0-ap-northeast-2.pooler.supabase.com:6543/postgres",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("port 6543"));
        assert!(!error.contains("do-not-leak"));
    }

    #[test]
    fn rejects_invalid_dsn_without_echoing_it() {
        let error = validate_database_url("not-a-dsn-with-secret")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a valid PostgreSQL DSN"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn does_not_reject_an_unrelated_postgres_host_by_port_alone() {
        validate_database_url("postgres://app:secret@postgres.internal:6543/app").unwrap();
    }
}
