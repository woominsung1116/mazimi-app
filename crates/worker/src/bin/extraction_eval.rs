//! Offline evaluation of AI fact extraction (A-09, Phase 0). No database,
//! no `.env`, no user data: it reads a labeled fixture of public program text
//! and prints per-field precision/recall, evidence validity and latency.
//!
//!   extraction-eval                                  # mock oracle: harness + label lint
//!   extraction-eval --provider ollama --model qwen3.5:9b-q4_K_M --url http://127.0.0.1:11434
//!   extraction-eval --fixture labels.json --provider ollama --gate --out report.json
//!
//! `--num-thread` (Ollama `options.num_thread`, default: CPUs available to
//! this process) must match the Ollama container's cpuset: Ollama otherwise
//! spawns a thread per host core and oversubscribes the cpuset.
//!
//! `--gate` exits non-zero below the rollout thresholds in
//! docs/mazimi-ai-extraction.md. `--auto-gate` checks the auto-verified
//! subset per type (`--auto-types`, default the auto-apply default scope):
//! precision ≥ 0.98 with at least 20 facts per type before that type may be
//! auto-applied. Developer/CI tool; not shipped in the image.
use mazimi_worker::ai_extraction::{
    auto::{parse_types, DEFAULT_TYPES},
    eval::{auto_gate, gate, parse_fixture, run, SAMPLE_FIXTURE},
    provider::{ExtractionProvider, MockExtractionProvider, OllamaExtractionProvider},
    source::build_source,
    DEFAULT_MODEL,
};
use mazimi_worker::ollama::{default_num_threads, valid_num_threads};
use std::time::Duration;

const USAGE: &str = "Usage: extraction-eval [--fixture PATH] [--provider mock|ollama] [--model NAME] [--url http://127.0.0.1:11434] [--allow-any-port] [--timeout-seconds 5..600] [--num-thread 1..64] [--limit N] [--out PATH] [--gate] [--print-sources] [--min-precision 0.95] [--min-recall 0.60] [--min-evidence 0.98] [--auto-gate] [--auto-types age,application_window,document] [--min-auto-precision 0.98] [--min-auto-support 20]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut fixture_path = None;
    let mut provider_name = "mock".to_string();
    let mut model = DEFAULT_MODEL.to_string();
    let mut url = "http://127.0.0.1:11434".to_string();
    let mut allow_any_port = false;
    let mut timeout = 180u64;
    let mut num_threads = default_num_threads();
    let mut limit = usize::MAX;
    let mut out = None;
    let mut gated = false;
    let mut print_sources = false;
    let (mut min_precision, mut min_recall, mut min_evidence) = (0.95, 0.60, 0.98);
    let mut auto_gated = false;
    let mut auto_types = parse_types(DEFAULT_TYPES)?;
    let (mut min_auto_precision, mut min_auto_support) = (0.98, 20usize);
    let value = |args: &mut std::iter::Skip<std::env::Args>, flag: &str| {
        args.next()
            .ok_or_else(|| anyhow::anyhow!("{flag} needs a value\n{USAGE}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => fixture_path = Some(value(&mut args, &arg)?),
            "--provider" => provider_name = value(&mut args, &arg)?,
            "--model" => model = value(&mut args, &arg)?,
            "--url" => url = value(&mut args, &arg)?,
            "--allow-any-port" => allow_any_port = true,
            "--timeout-seconds" => timeout = value(&mut args, &arg)?.parse()?,
            "--num-thread" => num_threads = value(&mut args, &arg)?.parse()?,
            "--limit" => limit = value(&mut args, &arg)?.parse()?,
            "--out" => out = Some(value(&mut args, &arg)?),
            "--gate" => gated = true,
            "--print-sources" => print_sources = true,
            "--min-precision" => min_precision = value(&mut args, &arg)?.parse()?,
            "--min-recall" => min_recall = value(&mut args, &arg)?.parse()?,
            "--min-evidence" => min_evidence = value(&mut args, &arg)?.parse()?,
            "--auto-gate" => auto_gated = true,
            "--auto-types" => auto_types = parse_types(&value(&mut args, &arg)?)?,
            "--min-auto-precision" => min_auto_precision = value(&mut args, &arg)?.parse()?,
            "--min-auto-support" => min_auto_support = value(&mut args, &arg)?.parse()?,
            _ => anyhow::bail!("{USAGE}"),
        }
    }
    anyhow::ensure!(
        (5..=600).contains(&timeout),
        "timeout must be 5..600 seconds"
    );
    anyhow::ensure!(valid_num_threads(num_threads), "num-thread must be 1..64");
    let raw = match &fixture_path {
        Some(path) => std::fs::read_to_string(path)?,
        None => SAMPLE_FIXTURE.to_string(),
    };
    let mut fixture = parse_fixture(&raw).map_err(anyhow::Error::msg)?;
    fixture.programs.truncate(limit.max(1));
    if print_sources {
        // The exact masked text labels must quote (char offsets index it).
        for program in &fixture.programs {
            let source = build_source(&program.source);
            println!("=== {} ({})\n{}\n", program.id, source.hash, source.text);
        }
        return Ok(());
    }

    let (provider, oracle): (Box<dyn ExtractionProvider>, bool) = match provider_name.as_str() {
        "mock" => (
            Box::new(MockExtractionProvider::new("mock-oracle", |_| {
                Ok(serde_json::json!({}))
            })),
            true,
        ),
        // Same private-endpoint allowlist as the worker and the API.
        "ollama" => (
            Box::new(
                OllamaExtractionProvider::new(
                    &url,
                    &model,
                    Duration::from_secs(timeout),
                    "30m",
                    allow_any_port,
                    num_threads,
                )
                .map_err(|_| {
                    anyhow::anyhow!(
                        "--url must be a private Ollama endpoint and --model a valid tag"
                    )
                })?,
            ),
            false,
        ),
        _ => anyhow::bail!("{USAGE}"),
    };
    let report = run(&fixture, provider.as_ref(), oracle).await;
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = &out {
        std::fs::write(path, &json)?;
    }
    println!(
        "{:<20} {:>4} {:>4} {:>4} {:>9} {:>7}",
        "field", "tp", "fp", "fn", "precision", "recall"
    );
    let fmt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{:.3}", v));
    for (field, s) in report
        .fields
        .iter()
        .chain([(&"overall".to_string(), &report.overall)])
    {
        println!(
            "{:<20} {:>4} {:>4} {:>4} {:>9} {:>7}",
            field,
            s.tp,
            s.fp,
            s.fn_,
            fmt(s.precision),
            fmt(s.recall)
        );
    }
    println!(
        "\n{:<20} {:>4} {:>4} {:>4} {:>9} {:>8}   (auto-verified subset: what auto-apply would write)",
        "auto_verified", "tp", "fp", "fn", "precision", "applied"
    );
    for (field, s) in &report.auto_verified {
        println!(
            "{:<20} {:>4} {:>4} {:>4} {:>9} {:>8}",
            field,
            s.tp,
            s.fp,
            s.fn_,
            fmt(s.precision),
            fmt(s.recall)
        );
    }
    println!(
        "evidence_validity={} valid_rate={} provider_failures={} latency_ms={}",
        fmt(report.evidence_validity_rate),
        fmt(report.valid_rate),
        report.provider_failures,
        report.latency_ms
    );
    if !fixture.meta.human_verified {
        println!("WARNING: fixture labels are not human-verified ({}); numbers are not a release measurement.", fixture.meta.labeler);
    }
    if oracle {
        println!(
            "NOTE: mock oracle replays the labels; it checks the harness and label validity only."
        );
    }
    if gated || auto_gated {
        if oracle || !fixture.meta.human_verified {
            println!("GATE: not applicable (needs --provider ollama and a human-verified fixture)");
            std::process::exit(2);
        }
        let mut failed = false;
        if gated {
            let failures = gate(&report, min_precision, min_recall, min_evidence);
            if failures.is_empty() {
                println!("GATE PASSED");
            } else {
                println!("GATE FAILED: {}", failures.join("; "));
                failed = true;
            }
        }
        if auto_gated {
            let failures = auto_gate(&report, &auto_types, min_auto_precision, min_auto_support);
            if failures.is_empty() {
                println!("AUTO GATE PASSED for {auto_types:?}");
            } else {
                println!("AUTO GATE FAILED: {}", failures.join("; "));
                failed = true;
            }
        }
        if failed {
            std::process::exit(1);
        }
    }
    Ok(())
}
