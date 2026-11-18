//! Shadow projection of the P2 intake over a candidate export (no database).
//!
//! ```text
//! corporate-intake-projection --input candidates.json --existing existing.json \
//!   --out projection.json [--contact-url https://…/crawler-policy] [--concurrency N] [--gray-tos]
//!   [--discover]
//! ```
//!
//! With `--discover`, link discovery runs first (each domain's homepage and
//! up to three exported candidates as seeds) and the projection covers the
//! offer pages it finds instead of the export.
//!
//! Applies the intake gates up to dedup to every exported candidate: policy,
//! reader A, reader B, scope, display and dedup against the exported live
//! programmes. Stability needs a second sighting a day later, so a source
//! that passes everything ends as `awaiting_stability`. Fetches politely with
//! the honest `MazimiBenefitBot` user agent (10 s floor or Crawl-delay per
//! domain); no page body is written. G1 (generic ToS) is off unless
//! `--gray-tos`, as in production.

use anyhow::{bail, Context, Result};
use chrono::{FixedOffset, Utc};
use mazimi_worker::corporate_auto::{
    bootstrap::{self, CandidateRow},
    http::PinnedHttp,
    intake::{self, Existing},
};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut input, mut existing, mut out) = (None, None, PathBuf::from("intake-projection.json"));
    let mut contact = std::env::var("CORPORATE_BOT_CONTACT_URL").ok();
    let (mut concurrency, mut gray_tos, mut discovery) = (8usize, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(args.next().context("--input needs a value")?)),
            "--existing" => {
                existing = Some(PathBuf::from(
                    args.next().context("--existing needs a value")?,
                ))
            }
            "--out" => out = PathBuf::from(args.next().context("--out needs a value")?),
            "--contact-url" => contact = Some(args.next().context("--contact-url needs a value")?),
            "--concurrency" => concurrency = args.next().context("value")?.parse()?,
            "--gray-tos" => gray_tos = true,
            "--discover" => discovery = true,
            other => bail!("unknown argument {other}"),
        }
    }
    let input = input.context("--input is required")?;
    let rows: Vec<CandidateRow> = serde_json::from_slice(&std::fs::read(&input)?)
        .with_context(|| format!("failed to read {}", input.display()))?;
    bootstrap::check_input(&rows)?;
    let existing: Vec<Existing> = match existing {
        Some(path) => serde_json::from_slice(&std::fs::read(&path)?)?,
        None => Vec::new(),
    };
    let contact = contact.context("a contact URL is required")?;
    let http = PinnedHttp::new(
        bootstrap::bootstrap_user_agent(contact.trim())?,
        Duration::from_secs(20),
    )?;
    let kst = FixedOffset::east_opt(9 * 3600).expect("valid offset");
    let today = Utc::now().with_timezone(&kst).date_naive();
    let (found, projected) = if discovery {
        let (found, projected) =
            intake::project_discovery(&http, rows, &existing, gray_tos, today, concurrency).await?;
        (Some(found), projected)
    } else {
        (
            None,
            intake::project(&http, rows, &existing, gray_tos, today, concurrency).await?,
        )
    };
    if let Some(found) = &found {
        println!("discovered {} offer pages", found.len());
    }
    let mut funnel: BTreeMap<String, usize> = BTreeMap::new();
    for p in &projected {
        *funnel
            .entry(format!("{}:{}", p.gate, p.outcome))
            .or_default() += p.candidate_id.split(',').count();
    }
    for (k, v) in &funnel {
        println!("{v:5} {k}");
    }
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(
            &serde_json::json!({"today": today, "funnel": funnel, "discovered": found, "decisions": projected}),
        )?,
    )?;
    println!("wrote {}", out.display());
    Ok(())
}
