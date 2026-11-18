//! One bounded optional LLM enrichment pass for managed cron platforms.

use mazimi_worker::runtime;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let pool = runtime::connect_job_pool().await?;

    match mazimi_worker::enrichment::run_pending_from_env(&pool).await? {
        Some(summary) => println!("{}", serde_json::to_string_pretty(&summary)?),
        None => println!(
            "{}",
            serde_json::json!({
                "enabled": false,
                "message": "BENEFIT_LLM_ENABLED is false; no external request was made"
            })
        ),
    }

    pool.close().await;
    Ok(())
}
