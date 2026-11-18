//! Read-only stored-rule health for the pre-deploy check: prints the counts
//! of [`mazimi_worker::rule_health::measure`] as JSON and exits non-zero
//! while any active rule fails to parse (such a hard filter hides its row).
//! Writes nothing; data maintenance runs the same measure with its event.

use mazimi_worker::{rule_health, runtime};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let pool = runtime::connect_job_pool().await?;
    let health = rule_health::measure(&pool).await?;
    pool.close().await;
    println!("{}", serde_json::to_string_pretty(&health)?);
    anyhow::ensure!(
        health.unparseable() == 0,
        "{} active programme(s) have a rule the parser rejects",
        health.unparseable()
    );
    Ok(())
}
