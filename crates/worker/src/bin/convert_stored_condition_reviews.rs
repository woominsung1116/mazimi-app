//! Rebuild import lines for `import-condition-reviews` from stored
//! `complete` condition reviews written before the current coverage check
//! version (`row_to_json(program_condition_reviews)`, one row per line).
//! Offline: no database, no network. See
//! `mazimi_core::condition_review::reimport`.

use std::io::Write;

use anyhow::{bail, Context};
use mazimi_core::condition_review::{coverage, reimport};
use serde_json::json;

struct Args {
    file: String,
    out: String,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let (mut file, mut out) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--file" => file = Some(value()?),
            "--out" => out = Some(value()?),
            "-h" | "--help" => {
                println!("Usage: convert-stored-condition-reviews --file stored.jsonl --out reimport.jsonl");
                return Ok(None);
            }
            unexpected => bail!("unknown argument: {unexpected}"),
        }
    }
    Ok(Some(Args {
        file: file.context("--file is required")?,
        out: out.context("--out is required")?,
    }))
}

fn main() -> anyhow::Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let text =
        std::fs::read_to_string(&args.file).with_context(|| format!("reading {}", args.file))?;
    let converted = reimport::convert(&text, coverage::VERSION);
    let mut file =
        std::fs::File::create(&args.out).with_context(|| format!("creating {}", args.out))?;
    for line in &converted.lines {
        writeln!(file, "{line}")?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "coverage_version": coverage::VERSION,
            "lines": converted.lines.len(),
            "skipped_by_reason": converted.skipped_by_reason,
            "skipped": converted.skipped,
        }))?
    );
    Ok(())
}
