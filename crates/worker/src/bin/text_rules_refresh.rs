//! Run the rule_v1 text-rule refresh until nothing is pending, in bounded
//! runs (the worker runs one bounded run every ten minutes). `--offline FILE` reads a JSONL export of
//! `{id, source_type, raw}` rows instead (raw = the projected payload keys of
//! `mazimi_core::text_rules::hashed_keys`), writes nothing, and prints one
//! JSON line per row: whether its text is fully structured and evaluable.

use anyhow::{bail, Context};
use mazimi_worker::{runtime, text_rules};
use std::io::{BufRead, Write};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => {
            let pool = runtime::connect_job_pool().await?;
            // Bounded runs until nothing is pending, or a run writes nothing
            // (only failing rows left: they are reported, not retried forever).
            loop {
                let summary = text_rules::refresh::refresh(&pool).await?;
                println!("{summary:?}");
                if summary.remaining() == 0 || summary.written == 0 {
                    break;
                }
            }
            pool.close().await;
        }
        Some("--offline") => {
            let path = args.next().context("--offline requires a file")?;
            let file = std::fs::File::open(&path).with_context(|| format!("open {path}"))?;
            let mut out = std::io::stdout().lock();
            for line in std::io::BufReader::new(file).lines() {
                let row: serde_json::Value = serde_json::from_str(&line?)?;
                let source = row["source_type"].as_str().unwrap_or_default();
                let c = text_rules::refresh::compile_row(source, &row["raw"]);
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "id": row["id"], "source_type": source, "has_text": c.has_text,
                        "fully_structured": c.fully_structured, "evaluable": c.evaluable,
                        "blockers": c.blockers, "leaves": c.leaves, "facts": c.facts,
                    })
                )?;
            }
        }
        Some("-h" | "--help") => {
            println!("Usage: text-rules-refresh [--offline FILE.jsonl]");
        }
        Some(other) => bail!("unknown argument: {other}"),
    }
    Ok(())
}
