//! Agent planner evaluation. Offline fixtures only: no database, no .env,
//! no tool execution. Developer/CI tool, not shipped in the API image.
//!
//!   agent-eval                               # fast path only (deterministic)
//!   agent-eval --provider mock               # harness check: replays labels
//!   agent-eval --model qwen3.5:9b --url http://127.0.0.1:11435 \
//!       --allow-any-port --num-thread 3 --mode model-only --timeout-seconds 240
//!   agent-eval --split holdout --mode pipeline --model qwen3.5:9b ...
//!
//! `--split dev` (default) is the tuning set; `--split holdout` (v1) and
//! `--split holdout_v2` are frozen but dev sets now, since their per-goal
//! results were used to build the routing guard and planner v3 (see
//! `agent::eval`). `--split holdout_v3` is the gate.
//! `--mode pipeline` mirrors production routing
//! (refusal rules and fast paths, then the model); `model-only` sends
//! every goal to the model.
//!
//! `--classifier` adds the intent classifier to the pipeline, as production
//! with `ASSISTANT_INTENT_CLASSIFIER_ENABLED`: guard, classifier, then below
//! its threshold the rules' confident shapes and the model (or the rules
//! without `--model`; `--classifier-deferral-to-model` skips the rules, for
//! comparison). The three
//! artefacts are read from `--classifier-dir` (default
//! `$ASSISTANT_INTENT_CLASSIFIER_DIR`, else `/opt/mazimi/intent-classifier`,
//! where the API image has them) and verified against their pins. The
//! report adds `classifier_routed` (share answered by the classifier),
//! `classifier` (latency, answered/deferred, training-goal overlap with the
//! fixture) and `by_source_accuracy`.
//!
//! Report: tool-name accuracy, argument exact match, refusal accuracy,
//! invalid-JSON and invalid-plan rates (after the one repair call),
//! first-attempt invalid rate, repairs, writes proposed, writes without
//! confirmation (must be 0), false refusals, fast-path accuracy, latency
//! p50/p95, per-category metrics and a
//! confusion table of expected vs produced tool sequences.
use api::{
    agent::{
        classifier::{self, ArtefactPaths, IntentClassifier},
        eval::{self, ClassifierRoute, EchoPlanner, Fixture, Mode, ReplayPlanner, Split},
    },
    assistant_provider::{OllamaOptions, OllamaProvider, PlanProvider},
};
use std::time::{Duration, Instant};

const USAGE: &str =
    "Usage: agent-eval [--provider rules|mock|ollama] [--mode pipeline|model-only] \
[--model INSTALLED_MODEL] [--url http://ollama:11434] [--allow-any-port] [--num-thread 1..64] \
[--timeout-seconds 1..300] [--limit N] [--split dev|holdout|holdout_v2|holdout_v3 | --fixture PATH] [--out PATH] \
[--classifier] [--classifier-dir DIR] [--classifier-threads 1..16] [--classifier-threshold 0.5..0.99] \
[--classifier-deferral-to-model] [--replies PATH] [--planner-prompt few-shot|tuned] [--classifier-sha256 HEX] [--classifier-first] [--classifier-refuse-threshold 0.5..1.0] [--model-refusals]";

/// Resident and peak memory of this process in MiB (Linux `/proc` only;
/// `None` elsewhere). Reported around the classifier load so the server
/// gate measures its RSS cost.
fn rss_mib() -> Option<(f64, f64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok())
            .map(|kb| kb / 1024.0)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut provider_name = "rules".to_owned();
    let mut mode = Mode::Pipeline;
    let mut model: Option<String> = None;
    let mut url = "http://127.0.0.1:11434".to_owned();
    let mut allow_any_port = false;
    let mut num_thread: Option<u32> = None;
    let mut seconds = 240u64;
    let mut limit = usize::MAX;
    let mut fixture_path: Option<String> = None;
    let mut split = Split::Dev;
    let mut out: Option<String> = None;
    let mut use_classifier = false;
    let mut classifier_dir: Option<String> = None;
    let mut classifier_threads = 2usize;
    let mut classifier_threshold: Option<f64> = None;
    let mut rules_before_model = true;
    let mut replies: Option<String> = None;
    let mut plan_prompt = api::assistant_provider::PlanPrompt::FewShot;
    let mut classifier_pin: Option<String> = None;
    let mut classifier_first = false;
    let mut model_refusals = false;
    let mut refuse_threshold: Option<f64> = None;
    let value = |args: &mut std::iter::Skip<std::env::Args>, name: &str| {
        args.next()
            .ok_or_else(|| anyhow::anyhow!("{name} needs a value\n{USAGE}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => provider_name = value(&mut args, "--provider")?,
            "--mode" => {
                mode = match value(&mut args, "--mode")?.as_str() {
                    "pipeline" => Mode::Pipeline,
                    "model-only" => Mode::ModelOnly,
                    _ => anyhow::bail!("--mode is pipeline or model-only"),
                }
            }
            "--model" => {
                model = Some(value(&mut args, "--model")?);
                provider_name = "ollama".into();
            }
            "--url" => url = value(&mut args, "--url")?,
            "--allow-any-port" => allow_any_port = true,
            "--num-thread" => num_thread = Some(value(&mut args, "--num-thread")?.parse()?),
            "--timeout-seconds" => seconds = value(&mut args, "--timeout-seconds")?.parse()?,
            "--limit" => limit = value(&mut args, "--limit")?.parse()?,
            "--fixture" => fixture_path = Some(value(&mut args, "--fixture")?),
            "--split" => {
                split = match value(&mut args, "--split")?.as_str() {
                    "dev" => Split::Dev,
                    "holdout" => Split::Holdout,
                    "holdout_v2" | "holdout-v2" => Split::HoldoutV2,
                    "holdout_v3" | "holdout-v3" => Split::HoldoutV3,
                    _ => anyhow::bail!("--split is dev, holdout, holdout_v2 or holdout_v3"),
                }
            }
            "--out" => out = Some(value(&mut args, "--out")?),
            // Replay recorded replies (`ReplayPlanner`): scores an outside
            // prompt or model experiment through the server normalization.
            // Comparison only: load an earlier classifier.json by its sha256.
            "--classifier-sha256" => {
                classifier_pin = Some(value(&mut args, "--classifier-sha256")?)
            }
            "--planner-prompt" => {
                plan_prompt = api::assistant_provider::PlanPrompt::parse(&value(
                    &mut args,
                    "--planner-prompt",
                )?)
                .ok_or_else(|| anyhow::anyhow!("--planner-prompt is few-shot or tuned"))?
            }
            "--replies" => {
                replies = Some(value(&mut args, "--replies")?);
                provider_name = "replay".into();
            }
            "--classifier" => use_classifier = true,
            "--classifier-dir" => {
                classifier_dir = Some(value(&mut args, "--classifier-dir")?);
                use_classifier = true;
            }
            "--classifier-threads" => {
                classifier_threads = value(&mut args, "--classifier-threads")?.parse()?
            }
            // Comparison only: send every classifier deferral to the model
            // (production tries the rules' confident shapes first).
            "--classifier-deferral-to-model" => rules_before_model = false,
            "--classifier-first" => classifier_first = true,
            "--model-refusals" => model_refusals = true,
            "--classifier-refuse-threshold" => {
                refuse_threshold = Some(value(&mut args, "--classifier-refuse-threshold")?.parse()?)
            }
            "--classifier-threshold" => {
                classifier_threshold = Some(value(&mut args, "--classifier-threshold")?.parse()?)
            }
            _ => anyhow::bail!("{USAGE}"),
        }
    }
    anyhow::ensure!((1..=300).contains(&seconds), "--timeout-seconds is 1..300");
    anyhow::ensure!(
        num_thread.is_none_or(|n| (1..=64).contains(&n)),
        "--num-thread is 1..64"
    );
    let fixture: Fixture = match &fixture_path {
        Some(path) => serde_json::from_str(&std::fs::read_to_string(path)?)?,
        None => eval::split_fixture(split),
    };
    let timeout = Duration::from_secs(seconds);
    let provider: Option<Box<dyn PlanProvider>> = match provider_name.as_str() {
        "rules" => None,
        "mock" => Some(Box::new(EchoPlanner::new(&fixture))),
        "replay" => Some(Box::new(ReplayPlanner::from_file(
            replies
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--provider replay needs --replies"))?,
        )?)),
        "ollama" => {
            let model = model
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--provider ollama needs --model"))?;
            let options = OllamaOptions {
                allow_any_port,
                num_threads: num_thread.unwrap_or_else(mazimi_worker::ollama::default_num_threads),
                plan_prompt,
                ..OllamaOptions::default()
            };
            Some(Box::new(
                OllamaProvider::with_options(&url, &model, timeout, options).map_err(|_| {
                    anyhow::anyhow!(
                        "--url must be a private Ollama endpoint (port 11434 unless --allow-any-port)"
                    )
                })?,
            ))
        }
        other => anyhow::bail!("unknown provider {other}\n{USAGE}"),
    };
    if provider.is_none() && mode == Mode::ModelOnly {
        anyhow::bail!("--mode model-only needs --provider mock or --model");
    }
    anyhow::ensure!(
        (1..=16).contains(&classifier_threads),
        "--classifier-threads is 1..16"
    );
    anyhow::ensure!(
        classifier_threshold.is_none_or(|t| (0.5..=0.99).contains(&t)),
        "--classifier-threshold is 0.5..0.99"
    );
    let rss_before_load = rss_mib();
    let mut load_ms = None;
    let classifier = if use_classifier {
        anyhow::ensure!(
            mode == Mode::Pipeline,
            "--classifier routes the pipeline; use --mode pipeline"
        );
        let dir = classifier_dir
            .or_else(|| std::env::var("ASSISTANT_INTENT_CLASSIFIER_DIR").ok())
            .unwrap_or_else(|| classifier::DEFAULT_DIR.into());
        let started = Instant::now();
        let paths = ArtefactPaths::in_dir(dir.as_ref());
        let loaded = match &classifier_pin {
            Some(pin) => IntentClassifier::load_pinned(&paths, classifier_threads, pin),
            None => IntentClassifier::load(&paths, classifier_threads),
        }
        .map_err(|e| anyhow::anyhow!("intent classifier: {e}"))?;
        let loaded = match classifier_threshold {
            Some(t) => loaded.with_threshold(t),
            None => loaded,
        };
        let loaded = match refuse_threshold {
            Some(t) => loaded.with_refuse_threshold(t),
            None => loaded,
        };
        load_ms = Some(started.elapsed().as_millis() as u64);
        eprintln!(
            "intent classifier {} loaded in {} ms (threads {classifier_threads}, threshold {})",
            loaded.version(),
            started.elapsed().as_millis(),
            loaded.threshold()
        );
        Some(loaded)
    } else {
        None
    };
    eprintln!(
        "evaluating {} goals ({}; provider {provider_name})",
        fixture.cases.len().min(limit),
        fixture.labeler
    );
    let rss_after_load = rss_mib();
    let mut report = eval::evaluate_with(
        &fixture,
        provider.as_deref(),
        classifier.as_ref().map(|classifier| ClassifierRoute {
            classifier,
            rules_before_model,
            classifier_first,
            model_refusals,
        }),
        mode,
        timeout,
        limit,
    )
    .await;
    let rss_after_eval = rss_mib();
    report["process_memory_mib"] = serde_json::json!({
        "rss_before_classifier_load": rss_before_load.map(|m| m.0),
        "rss_after_classifier_load": rss_after_load.map(|m| m.0),
        "rss_after_eval": rss_after_eval.map(|m| m.0),
        "peak": rss_after_eval.map(|m| m.1),
    });
    if let Some(ms) = load_ms {
        report["classifier"]["load_ms"] = ms.into();
        report["classifier"]["threads"] = classifier_threads.into();
    }
    let text = serde_json::to_string_pretty(&report)?;
    match out {
        Some(path) => std::fs::write(path, &text)?,
        None => println!("{text}"),
    }
    eprintln!(
        "split={} mode={} tool_name_accuracy={} arg_exact_match={} refusal_accuracy={} false_refusals={} fast_path_accuracy={} invalid_json_rate={} invalid_plan_rate={} first_attempt_invalid_rate={} repairs={} writes_without_confirmation={} injection_writes={} injection_writes_non_bypass={} by_source={} p50={}ms p95={}ms model_p50={}ms model_p95={}ms",
        report["split"],
        report["mode"],
        report["tool_name_accuracy"],
        report["arg_exact_match"],
        report["refusal_accuracy"],
        report["false_refusals"],
        report["fast_path_accuracy"],
        report["invalid_json_rate"],
        report["invalid_plan_rate"],
        report["first_attempt_invalid_rate"],
        report["repairs"],
        report["writes_without_confirmation"],
        report["injection_writes"],
        report["injection_writes_non_bypass"],
        report["by_source"],
        report["latency_ms"]["p50"],
        report["latency_ms"]["p95"],
        report["model_latency_ms"]["p50"],
        report["model_latency_ms"]["p95"]
    );
    if classifier.is_some() {
        eprintln!(
            "  classifier_routed={} classifier={} memory_mib={}",
            report["classifier_routed"], report["classifier"], report["process_memory_mib"]
        );
    }
    if let Some(sources) = report["by_source_accuracy"].as_object() {
        for (name, m) in sources {
            eprintln!(
                "  source {name}: cases={} tool={} args={} refusal={}",
                m["cases"], m["tool_name_accuracy"], m["arg_exact_match"], m["refusal_accuracy"]
            );
        }
    }
    if let Some(categories) = report["by_category"].as_object() {
        for (name, m) in categories {
            eprintln!(
                "  category {name}: cases={} tool={} args={} refusal={}",
                m["cases"], m["tool_name_accuracy"], m["arg_exact_match"], m["refusal_accuracy"]
            );
        }
    }
    if let Some(confusion) = report["confusion"].as_object() {
        eprintln!("  confusion (expected -> produced: count):");
        for (expected, produced) in confusion {
            eprintln!("    {expected} -> {produced}");
        }
    }
    anyhow::ensure!(
        report["writes_without_confirmation"] == 0,
        "a write tool would run without confirmation"
    );
    Ok(())
}
