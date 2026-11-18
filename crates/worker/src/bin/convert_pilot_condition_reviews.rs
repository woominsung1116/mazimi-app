//! Convert the verified-conditions pilot's merged reviews
//! (`verified-p0/reviews.jsonl`) into import files for
//! `import-condition-reviews` and `import-applicant-reviews`. Offline: no
//! database, no network. See `mazimi_core::condition_review::pilot`.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{bail, Context};
use mazimi_core::condition_review::pilot;
use serde_json::{json, Value};

struct Args {
    file: String,
    meta_dir: String,
    reviewer: String,
    out_conditions: String,
    out_applicants: String,
}

fn parse_args() -> anyhow::Result<Option<Args>> {
    let (mut file, mut meta_dir, mut reviewer, mut out_conditions, mut out_applicants) =
        (None, None, None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--file" => file = Some(value()?),
            "--meta-dir" => meta_dir = Some(value()?),
            "--reviewer" => reviewer = Some(value()?),
            "--out-conditions" => out_conditions = Some(value()?),
            "--out-applicants" => out_applicants = Some(value()?),
            "-h" | "--help" => {
                println!("Usage: convert-pilot-condition-reviews --file reviews.jsonl --meta-dir meta/ --reviewer NAME --out-conditions conditions.jsonl --out-applicants applicants.jsonl");
                return Ok(None);
            }
            unexpected => bail!("unknown argument: {unexpected}"),
        }
    }
    Ok(Some(Args {
        file: file.context("--file is required")?,
        meta_dir: meta_dir.context("--meta-dir is required")?,
        reviewer: reviewer.context("--reviewer is required")?,
        out_conditions: out_conditions.context("--out-conditions is required")?,
        out_applicants: out_applicants.context("--out-applicants is required")?,
    }))
}

/// program_id -> applicant text hash from the pilot's `meta/<id>.json`.
fn applicant_hashes(meta_dir: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(meta_dir).with_context(|| format!("reading {meta_dir}"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("parsing {}", path.display()))?;
        if let (Some(id), Some(hash)) = (
            meta["program_id"].as_str(),
            meta["applicant_text_hash"].as_str(),
        ) {
            out.insert(id.to_owned(), hash.to_owned());
        }
    }
    Ok(out)
}

fn write_lines(path: &str, lines: &[Value]) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path).with_context(|| format!("creating {path}"))?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let text =
        std::fs::read_to_string(&args.file).with_context(|| format!("reading {}", args.file))?;
    let hashes = applicant_hashes(&args.meta_dir)?;
    let out = pilot::convert(&text, args.reviewer.trim(), &hashes);
    write_lines(&args.out_conditions, &out.conditions)?;
    write_lines(&args.out_applicants, &out.applicants)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "conditions": out.conditions.len(),
            "applicants": out.applicants.len(),
            "by_status": out.by_status,
            "overrides_by_target": out.overrides_by_target,
            "skipped": out.skipped,
            "applicant_skipped": out.applicant_skipped,
        }))?
    );
    Ok(())
}
