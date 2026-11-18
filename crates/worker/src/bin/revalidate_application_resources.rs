//! Manual, one-off application-resource pass for operators (for example a
//! backfill right after enabling the feature). The scheduled revalidation is
//! the long-running `worker` cron (minute 35 of every hour); running this from
//! an external timer as well would double the probe rate per source domain,
//! so it refuses to start without `--manual`.
use mazimi_worker::{application_resources::Job, runtime};
use tracing_subscriber::EnvFilter;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    if !std::env::args().skip(1).any(|arg| arg == "--manual") {
        anyhow::bail!(
            "application resources are revalidated by the worker cron; pass --manual for a one-off operator run"
        );
    }
    let Some(job) = Job::from_env()? else {
        tracing::info!("APPLICATION_RESOURCES_ENABLED is false; no collection performed");
        return Ok(());
    };
    let pool = runtime::connect_job_pool().await?;
    let result = job.run(&pool).await?;
    tracing::info!(?result, "application resource revalidation finished");
    pool.close().await;
    Ok(())
}
