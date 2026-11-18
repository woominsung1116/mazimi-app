//! Bounded, network-free replay of retained ingestion payloads.

use anyhow::{bail, Context};
use mazimi_worker::{pipeline, runtime};
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct Args {
    source: Option<String>,
    limit: usize,
    regions_only: bool,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut source = None;
    let mut regions_only = false;
    let mut limit = 1_000usize;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--regions-only" => regions_only = true,
            "--source" => {
                let value = args.next().context("--source requires a value")?;
                if value.trim().is_empty() {
                    bail!("--source must not be blank");
                }
                source = Some(value);
            }
            "--limit" => {
                limit = args
                    .next()
                    .context("--limit requires a value")?
                    .parse()
                    .context("--limit must be a positive integer")?;
            }
            "-h" | "--help" => {
                println!("Usage: renormalize-snapshots [--source SOURCE_NAME] [--limit 1..100000] [--regions-only]");
                return Ok(None);
            }
            unexpected => bail!("unknown argument: {unexpected}"),
        }
    }

    Ok(Some(Args {
        source,
        limit,
        regions_only,
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let pool = runtime::connect_job_pool().await?;

    let summary = if args.regions_only {
        pipeline::renormalize_stored_regions(&pool, args.source.as_deref(), args.limit).await?
    } else {
        pipeline::renormalize_stored_snapshots(&pool, args.source.as_deref(), args.limit).await?
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);

    pool.close().await;
    Ok(())
}
