use mazimi_worker::runtime;
use mazimi_worker::sources::official_seeds::OfficialSeedDiscoverer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let pool = runtime::connect_job_pool().await?;

    let summary = OfficialSeedDiscoverer::run(&pool).await?;
    tracing::info!(
        targets = summary.targets,
        configured_urls = summary.configured_urls,
        candidates_refreshed = summary.candidates_refreshed,
        skipped = summary.skipped,
        "official seed candidate run finished"
    );
    pool.close().await;
    Ok(())
}
