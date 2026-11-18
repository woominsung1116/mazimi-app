//! One-shot source revalidation entry point for managed cron platforms.
//!
//! The long-running `worker` binary schedules the same operations internally.
//! Application-resource probes are not part of this run unless
//! `--with-application-resources` is passed (the worker cron owns them).
//! This binary exists so Render Cron Jobs, Kubernetes CronJobs, or an Oracle
//! systemd timer can invoke one bounded run without duplicating business logic.

use mazimi_worker::{
    data_maintenance, runtime, source_monitoring, sources::benefit_pages::BenefitPageFetcher,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let pool = runtime::connect_job_pool().await?;
    // Application resources are revalidated by the long-running worker cron.
    // This timer binary probes them only when explicitly asked to, so the two
    // schedules never run the same probes against one source domain.
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--with-application-resources")
    {
        if let Some(job) = mazimi_worker::application_resources::Job::from_env()? {
            let summary = job.run(&pool).await?;
            tracing::info!(?summary, "application resources checked");
        }
    }

    if let Some(fetcher) = BenefitPageFetcher::from_env()? {
        let summary = fetcher.run(&pool).await?;
        tracing::info!(
            claimed = summary.claimed,
            review = summary.ready_for_review,
            failed = summary.failed,
            blocked = summary.blocked,
            "one-shot approved source fetch complete"
        );
    } else {
        tracing::info!(
            "BENEFIT_CRAWLER_ENABLED is false; skipping direct fetch and enforcing freshness only"
        );
    }

    let (grace, limit) = source_monitoring::freshness_policy_from_env()?;
    let summary = source_monitoring::run_source_maintenance(&pool, grace, limit).await?;
    tracing::info!(
        schedules_reconciled = summary.schedules_reconciled,
        unmonitored_programs_held = summary.unmonitored_programs_held,
        overdue_candidates_held = summary.overdue_candidates_held,
        overdue_programs_held = summary.overdue_programs_held,
        "one-shot source maintenance sweep complete"
    );

    let data_summary = data_maintenance::run(&pool).await?;
    tracing::info!(
        duplicate_candidates_upserted = data_summary.duplicate_candidates_upserted,
        quality_issues_observed = data_summary.quality_issues_observed,
        programs_expired = data_summary.programs_expired,
        "one-shot corporate benefit data maintenance complete"
    );
    pool.close().await;
    Ok(())
}
