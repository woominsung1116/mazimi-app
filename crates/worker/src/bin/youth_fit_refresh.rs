//! One-shot youth_fit refresh (backfill after deploying a new classifier
//! version). The worker runs the same step inside data maintenance; this
//! binary runs only it and prints the verdict counts.

use mazimi_worker::{runtime, youth_fit};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let pool = runtime::connect_job_pool().await?;
    let summary = youth_fit::refresh_monitored(&pool)
        .await
        .ok_or_else(|| anyhow::anyhow!("youth_fit refresh failed"))?;
    tracing::info!(
        examined = summary.examined,
        written = summary.written,
        demoted = youth_fit::demoted(&summary),
        verdicts = ?summary.verdicts,
        "youth_fit refresh complete"
    );
    pool.close().await;
    Ok(())
}
