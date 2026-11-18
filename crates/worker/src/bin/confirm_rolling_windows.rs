//! Confirm rolling/unknown windows of complete 온통청년 condition reviews from
//! the payloads the daily 온통청년 sync stored, on demand (the worker also runs
//! it daily after that sync). No network request. Needs
//! `ROLLING_WINDOW_CONFIRMATION_ENABLED`. See `mazimi_worker::condition_reviews`.

use anyhow::{bail, Context};
use mazimi_worker::{condition_reviews, runtime};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut dry_run = false;
    let mut limit = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--limit" => {
                let value: u64 = args
                    .next()
                    .context("--limit requires a number")?
                    .parse()
                    .context("--limit must be a number")?;
                if !(1..=1_500).contains(&value) {
                    bail!("--limit must be between 1 and 1500");
                }
                limit = Some(value);
            }
            "-h" | "--help" => {
                println!("Usage: confirm-rolling-windows [--dry-run] [--limit N (1-1500)]");
                println!("Requires ROLLING_WINDOW_CONFIRMATION_ENABLED=true. Reads the stored 온통청년 sync (fresh within 36 h); makes no network request.");
                return Ok(());
            }
            unexpected => bail!("unknown argument: {unexpected}"),
        }
    }
    let Some(mut config) = condition_reviews::confirmation_config_from_env()? else {
        bail!("ROLLING_WINDOW_CONFIRMATION_ENABLED is not set; nothing was read or written");
    };
    if let Some(limit) = limit {
        config.max_programs = limit;
    }
    let pool = runtime::connect_job_pool().await?;
    let summary =
        condition_reviews::confirm_rolling_windows(&pool, &config, dry_run, chrono::Utc::now())
            .await?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    pool.close().await;
    Ok(())
}
