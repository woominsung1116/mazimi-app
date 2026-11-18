//! One-time corporate offer bootstrap (read-only toward the database).
//!
//! ```text
//! corporate-bootstrap --input candidates.json --out proposals.json \
//!   [--contact-url https://…/crawler-policy] [--concurrency N] [--no-gray-tos]
//! ```
//!
//! Reads the candidate export, applies the collection policy, fetches each
//! allowed page politely with the honest `MazimiBenefitBot` user agent and
//! writes proposals plus the funnel by reason. No page body is written.
//! Fails closed: an export row missing a policy field, or any page that
//! could not be read, exits non-zero and writes no (partial) report.
//! The contact URL defaults to `CORPORATE_BOT_CONTACT_URL`.

use anyhow::{bail, Context, Result};
use chrono::{FixedOffset, Utc};
use mazimi_worker::corporate_auto::{
    bootstrap::{self, CandidateRow, Settings},
    http::PinnedHttp,
};
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
    let mut input: Option<PathBuf> = None;
    let mut out = PathBuf::from("corporate-bootstrap.json");
    let mut contact = std::env::var("CORPORATE_BOT_CONTACT_URL").ok();
    let mut settings = Settings::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(args.next().context("--input needs a value")?)),
            "--out" => out = PathBuf::from(args.next().context("--out needs a value")?),
            "--contact-url" => contact = Some(args.next().context("--contact-url needs a value")?),
            "--concurrency" => settings.concurrency = args.next().context("value")?.parse()?,
            "--no-gray-tos" => settings.gray_generic_tos = false,
            other => bail!("unknown argument {other}"),
        }
    }
    let input = input.context("--input is required")?;
    let contact = contact
        .context("a contact URL is required (--contact-url or CORPORATE_BOT_CONTACT_URL)")?;
    let rows: Vec<CandidateRow> = serde_json::from_slice(&std::fs::read(&input)?)
        .with_context(|| format!("failed to read {}", input.display()))?;
    bootstrap::check_input(&rows)?;
    let http = PinnedHttp::new(
        bootstrap::bootstrap_user_agent(contact.trim())?,
        Duration::from_secs(20),
    )?;
    let kst = FixedOffset::east_opt(9 * 3600).expect("valid offset");
    let today = Utc::now().with_timezone(&kst).date_naive();
    let report = bootstrap::run(&http, rows, settings, today).await?;
    println!("{}", bootstrap::funnel_text(&report));
    bootstrap::write_report(&report, &out)?;
    println!("wrote {}", out.display());
    Ok(())
}
