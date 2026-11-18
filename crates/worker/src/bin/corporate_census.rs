//! P0 corporate census CLI (read-only toward sources and publication).
//!
//! ```text
//! CORPORATE_CENSUS_ENABLED=true CORPORATE_BOT_CONTACT_URL=https://… \
//!   corporate-census [--out-dir DIR] [--max-candidate-probes N] [--concurrency N]
//! corporate-census --events-dry-run [--out-dir DIR]
//! ```
//!
//! Writes `census-<run>.json` (summary + every entry) and
//! `census-<run>-summary.txt`, and stores the run in `corporate_census_*`
//! where the P1 promotion reads it. `--events-dry-run` makes no request: it
//! reports which open source-monitoring events evidence verification would
//! resolve and how.

use anyhow::{bail, Context, Result};
use mazimi_worker::corporate_auto::{census, events, http::PinnedHttp, Flags};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let mut out_dir = PathBuf::from("corporate-census");
    let mut settings = census::CensusSettings::default();
    let mut events_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out-dir" => out_dir = PathBuf::from(args.next().context("--out-dir needs a value")?),
            "--max-candidate-probes" => {
                settings.max_candidate_probes = args.next().context("value")?.parse()?;
            }
            "--concurrency" => settings.concurrency = args.next().context("value")?.parse()?,
            "--events-dry-run" => events_only = true,
            other => bail!("unknown argument {other}"),
        }
    }
    let pool = mazimi_worker::runtime::connect_worker_pool().await?;
    std::fs::create_dir_all(&out_dir)?;
    if events_only {
        let report = events::dry_run_report(&pool).await?;
        let path = out_dir.join("source-events-dry-run.json");
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        println!("{}", serde_json::to_string_pretty(&report["counts"])?);
        println!("wrote {}", path.display());
        return Ok(());
    }
    let flags = Flags::from_env()?;
    if !flags.census_enabled {
        bail!("CORPORATE_CENSUS_ENABLED is off");
    }
    let http = PinnedHttp::new(
        flags
            .user_agent()
            .context("CORPORATE_BOT_CONTACT_URL is required")?,
        Duration::from_secs(20),
    )?;
    let run = census::run(&pool, &flags, &http, &settings).await?;
    let json_path = out_dir.join(format!("census-{}.json", run.run_id));
    let text_path = out_dir.join(format!("census-{}-summary.txt", run.run_id));
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(
            &serde_json::json!({"summary": run.summary, "entries": run.entries}),
        )?,
    )?;
    std::fs::write(&text_path, census::summary_text(&run.summary))?;
    println!("{}", census::summary_text(&run.summary));
    println!("wrote {} and {}", json_path.display(), text_path.display());
    Ok(())
}
