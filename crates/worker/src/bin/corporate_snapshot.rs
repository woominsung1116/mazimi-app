//! Real-page snapshots of the published and audited-correct corporate
//! offers (handoff §18), for measuring hide_only before it is enabled.
//!
//! ```text
//! CORPORATE_BOT_CONTACT_URL=https://… corporate-snapshot [--out-dir DIR] [--targets FILE]
//! ```
//!
//! Read-only toward sources: robots.txt for our agent, the pinned HTTPS
//! client, per-domain spacing; blocked pages are skipped. No database.
//! Fails closed: if any page could not be read the run exits non-zero and
//! writes nothing (no page, no index).

use anyhow::{bail, Context, Result};
use mazimi_worker::corporate_auto::{http::PinnedHttp, snapshot, user_agent};
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
    let mut out_dir = PathBuf::from("crates/worker/tests/fixtures/real_pages");
    let mut targets: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out-dir" => out_dir = PathBuf::from(args.next().context("--out-dir needs a value")?),
            // A JSON list of {program_id, title, url, fragments}: the
            // known-ended set.
            "--targets" => {
                targets = Some(PathBuf::from(
                    args.next().context("--targets needs a value")?,
                ))
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let contact = std::env::var("CORPORATE_BOT_CONTACT_URL")
        .context("CORPORATE_BOT_CONTACT_URL is required (the honest bot identity)")?;
    let http = PinnedHttp::new(user_agent(contact.trim()), Duration::from_secs(20))?;
    let entries = match targets {
        Some(file) => {
            let list = std::fs::read_to_string(&file).context("--targets file")?;
            snapshot::run_targets(
                &http,
                &out_dir,
                serde_json::from_str(&list).context("--targets JSON")?,
            )
            .await?
        }
        None => snapshot::run(&http, &out_dir).await?,
    };
    let stored = entries.iter().filter(|e| e.outcome == "stored").count();
    println!(
        "{} pages, {stored} stored, written to {}",
        entries.len(),
        out_dir.display()
    );
    for entry in &entries {
        println!(
            "{:<10} {} {}",
            entry.outcome,
            &entry.program_id[..8],
            entry.url
        );
    }
    Ok(())
}
