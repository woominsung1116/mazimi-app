//! Import reviewed applicant classes (JSON lines) into
//! `program_applicant_reviews`. See `mazimi_worker::applicant_reviews`.

use std::io::Read;

use anyhow::{bail, Context};
use mazimi_worker::{applicant_reviews, runtime};
use tracing_subscriber::EnvFilter;

/// Largest accepted input; a review file is a few hundred short lines.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

struct Args {
    file: String,
    dry_run: bool,
    operator: Option<String>,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut file = None;
    let mut dry_run = false;
    let mut operator = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--operator" => operator = Some(args.next().context("--operator requires a name")?),
            "--file" => {
                file = Some(
                    args.next()
                        .context("--file requires a path (or - for stdin)")?,
                )
            }
            "-h" | "--help" => {
                println!("Usage: import-applicant-reviews --file <reviews.jsonl|-> [--dry-run] [--operator NAME (default $USER)]");
                return Ok(None);
            }
            unexpected => bail!("unknown argument: {unexpected}"),
        }
    }
    let Some(file) = file else {
        bail!("--file is required");
    };
    Ok(Some(Args {
        file,
        dry_run,
        operator,
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
    let mut text = String::new();
    let source_name = if args.file == "-" {
        std::io::stdin()
            .take(MAX_INPUT_BYTES + 1)
            .read_to_string(&mut text)
            .context("reading stdin")?;
        "stdin".to_owned()
    } else {
        std::fs::File::open(&args.file)
            .with_context(|| format!("opening {}", args.file))?
            .take(MAX_INPUT_BYTES + 1)
            .read_to_string(&mut text)
            .with_context(|| format!("reading {}", args.file))?;
        std::path::Path::new(&args.file)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
    };
    if text.len() as u64 > MAX_INPUT_BYTES {
        bail!("input larger than {MAX_INPUT_BYTES} bytes");
    }

    let actor = applicant_reviews::import_actor(
        args.operator.as_deref(),
        std::env::var("USER").ok(),
        &source_name,
    )
    .map_err(anyhow::Error::msg)?;

    let pool = runtime::connect_job_pool().await?;
    let mut connection = pool.acquire().await?;
    let summary =
        applicant_reviews::import_on(&mut connection, &text, &actor, args.dry_run).await?;
    drop(connection);
    println!("{}", serde_json::to_string_pretty(&summary)?);
    pool.close().await;
    if summary.rejected > 0 {
        bail!("{} line(s) rejected; nothing was written", summary.rejected);
    }
    Ok(())
}
