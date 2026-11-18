//! Lists, and with `--apply` marks `rejected`, candidate form templates the
//! collector attached to the wrong programme, and restores earlier automatic
//! rejections the current rules no longer support. Noise means recruitment
//! documents, board-wide attachments, earlier-round files and files from
//! unrelated linked pages. The hourly collector job applies the same rule before every
//! claim, so this is for an operator who wants to see the effect first.
//! Rows are never deleted. Prints ids and rule names only: labels and
//! titles are crawled text.
use mazimi_worker::{application_resources::reject_noise, runtime};
use std::collections::BTreeMap;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let apply = std::env::args().skip(1).any(|arg| arg == "--apply");
    let pool = runtime::connect_job_pool().await?;
    let sweep = reject_noise(&pool, apply).await?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (row, reason) in &sweep.rejected {
        *counts.entry(reason).or_default() += 1;
        println!("{} {reason}", row.id);
    }
    for row in &sweep.restored {
        *counts
            .entry("restored (no rule fires any more)")
            .or_default() += 1;
        println!("{} restored", row.id);
    }
    for (reason, n) in &counts {
        println!("{n:>5}  {reason}");
    }
    println!(
        "{:>5}  {}",
        sweep.rejected.len() + sweep.restored.len(),
        if apply {
            "changed"
        } else {
            "would change (dry run; pass --apply)"
        }
    );
    pool.close().await;
    Ok(())
}
