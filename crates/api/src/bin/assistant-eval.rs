//! Offline fixtures only. No .env loading, DB connection, real questions, or
//! model downloads. Defaults to deterministic rules; --model opts into
//! inference against an already installed Ollama model. Developer/CI tool:
//! not shipped in the runtime API image.
use api::assistant_provider::{rule_topics, IntentProvider, OllamaProvider, Topic};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Group {
    topics: Vec<Topic>,
    questions: Vec<String>,
}
const DEV: &str = include_str!("../../tests/fixtures/assistant_intents.json");
const HOLDOUT: &str = include_str!("../../tests/fixtures/assistant_intents_holdout.json");
const REGRESSION: &str = include_str!("../../tests/fixtures/assistant_intents_regression.json");

#[derive(Deserialize)]
struct Fixture {
    groups: Vec<Group>,
}
fn corpus(raw: &str) -> Vec<Group> {
    serde_json::from_str::<Fixture>(raw)
        .expect("checked-in evaluation corpus")
        .groups
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model = None;
    let mut limit = 100usize;
    let mut seconds = 15u64;
    let mut environment = "local".to_owned();
    let mut url = "http://127.0.0.1:11434".to_owned();
    let mut corpus_name = "dev".to_owned();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model=Some(args.next().ok_or_else(||anyhow::anyhow!("--model requires an installed model name"))?),
            "--limit" => limit=args.next().ok_or_else(||anyhow::anyhow!("missing limit"))?.parse()?,
            "--timeout-seconds" => seconds=args.next().ok_or_else(||anyhow::anyhow!("missing timeout"))?.parse()?,
            "--environment" => environment=args.next().ok_or_else(||anyhow::anyhow!("missing environment"))?,
            // Validated by the same private-endpoint allowlist as the API.
            "--url" => url=args.next().ok_or_else(||anyhow::anyhow!("missing url"))?,
            "--corpus" => corpus_name=args.next().ok_or_else(||anyhow::anyhow!("missing corpus"))?,
            _=>anyhow::bail!("Usage: assistant-eval [--corpus dev|holdout|regression] [--model INSTALLED_MODEL] [--url http://ollama:11434] [--limit 1..100] [--timeout-seconds 1..45] [--environment local|oracle]"),
        }
    }
    anyhow::ensure!(
        (1..=100).contains(&limit) && (1..=45).contains(&seconds),
        "invalid evaluation bounds"
    );
    anyhow::ensure!(
        ["local", "oracle"].contains(&environment.as_str()),
        "invalid environment label"
    );
    let raw = match corpus_name.as_str() {
        "dev" => DEV,
        "holdout" => HOLDOUT,
        "regression" => REGRESSION,
        _ => anyhow::bail!("invalid corpus: dev|holdout|regression"),
    };
    let timeout = Duration::from_secs(seconds);
    let provider = model
        .as_ref()
        .map(|name| OllamaProvider::new(&url, name, timeout))
        .transpose()
        .map_err(|_| anyhow::anyhow!("--url must be a private Ollama endpoint on port 11434 and --model a plain model name"))?;
    let mut rows = Vec::new();
    let mut latencies = Vec::new();
    let mut correct = 0;
    let mut failures = 0;
    for group in corpus(raw) {
        for question in group.questions {
            if rows.len() >= limit {
                break;
            }
            let start = Instant::now();
            let result = if let Some(provider) = &provider {
                match tokio::time::timeout(timeout, provider.classify(&question)).await {
                    Ok(value) => value,
                    Err(_) => Err(api::assistant_provider::ProviderError::Unavailable),
                }
            } else {
                Ok(rule_topics(&question))
            };
            let ms = start.elapsed().as_millis() as u64;
            latencies.push(ms);
            let exact = result.as_ref().is_ok_and(|topics| {
                topics.len() == group.topics.len()
                    && topics.iter().all(|topic| group.topics.contains(topic))
            });
            if exact {
                correct += 1;
            }
            let error = result.as_ref().err().map(ToString::to_string);
            if error.is_some() {
                failures += 1;
            }
            rows.push(json!({"case":rows.len()+1,"expected":group.topics,"actual":result.ok(),"exact":exact,"latency_ms":ms,"error":error}));
            if rows.len() % 10 == 0 {
                eprintln!("evaluated {}/{} synthetic cases", rows.len(), limit);
            }
        }
    }
    latencies.sort_unstable();
    let count = rows.len();
    println!(
        "{}",
        serde_json::to_string(
            &json!({"corpus":corpus_name,"synthetic_only":true,"model":model,"environment":environment,"timeout_seconds":seconds,"cases":count,"exact_matches":correct,"exact_match_rate":correct as f64/count as f64,"errors":failures,"p50_ms":latencies[(count-1)/2],"p95_ms":latencies[(count*95).div_ceil(100)-1],"results":rows})
        )?
    );
    Ok(())
}

#[test]
fn evaluation_corpus_has_one_hundred_unique_questions_and_bounded_topics() {
    let groups = corpus(DEV);
    let mut questions = std::collections::HashSet::new();
    for group in groups {
        assert!((1..=3).contains(&group.topics.len()));
        for question in group.questions {
            assert!(!question.trim().is_empty());
            assert!(questions.insert(question));
        }
    }
    assert_eq!(questions.len(), 100);
}

#[test]
fn eval_url_uses_the_api_allowlist() {
    let t = Duration::from_secs(1);
    assert!(OllamaProvider::new("http://ollama:11434", "m", t).is_ok());
    assert!(OllamaProvider::new("http://169.254.169.254:11434", "m", t).is_err());
    assert!(OllamaProvider::new("https://example.org", "m", t).is_err());
}
