//! Offline, read-only preview of a residence-scope mapping change.
//!
//! Reads JSON lines `{"source_type", "source_id", "regions", "raw"}` (an export
//! of `programs`: stored regions plus `raw_payload`) from stdin and recomputes
//! each scope with the current mapping. Prints one TSV row per changed scope
//! on stdout and the transition counts on stderr. No database access.
use mazimi_worker::regions::{self, UNKNOWN};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::BufRead;

fn kind(regions: &[String]) -> &'static str {
    if regions.is_empty() {
        "nationwide"
    } else if regions.iter().any(|r| r == UNKNOWN) {
        "unknown"
    } else {
        "region"
    }
}

fn main() -> anyhow::Result<()> {
    let mut counts: BTreeMap<(String, &str, &str), usize> = BTreeMap::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Ok(row) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let source = row["source_type"].as_str().unwrap_or("");
        let raw = &row["raw"];
        let scope = match source {
            "gov_benefits" => regions::government_scope(raw),
            "youth_center" => regions::youth_scope(raw),
            _ => continue,
        };
        let before: Vec<String> = row["regions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let (from, to) = (kind(&before), kind(&scope.regions));
        *counts.entry((source.into(), from, to)).or_default() += 1;
        if before != scope.regions {
            println!(
                "{source}\t{}\t{}\t{}\t{}\t{}\t{}",
                row["source_id"].as_str().unwrap_or(""),
                raw["소관기관유형"].as_str().unwrap_or(""),
                raw["소관기관명"].as_str().unwrap_or(""),
                before.join("+"),
                scope.regions.join("+"),
                scope.note.unwrap_or(""),
            );
        }
    }
    for ((source, from, to), n) in counts {
        eprintln!("{source}\t{from} -> {to}\t{n}");
    }
    Ok(())
}
