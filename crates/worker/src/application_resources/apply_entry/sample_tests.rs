//! Offline measurement over a production export: the job's own
//! [`super::evaluate`], run twice (a fresh request cache each pass, so the
//! second pass fetches again), over programmes read with
//! `SELECT ... FROM current_programs` and their 보조금24 detail. Writes one
//! JSON line per programme; nothing is stored except the robots.txt cache
//! of the test database.
//!
//! `APPLY_LINKS_SAMPLE=sample.jsonl APPLY_LINKS_SAMPLE_OUT=out.jsonl \
//!  TEST_DATABASE_URL=... cargo test -p mazimi-worker --lib apply_links_sample -- --ignored --nocapture`
//! (`APPLY_LINKS_JUDGE_MODEL`/`_URL`/`_ALLOW_ANY_PORT` turn on the judge.)
use super::{evaluate, fetch::Session, judge::Judge, Outcome, Programme};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn programme(row: &Value) -> Programme {
    let raw = row["raw"].clone();
    let program = json!({
        "id": row["id"], "program_type": "policy", "source_type": row["source_type"],
        "title": row["title"], "official_url": row["official_url"], "program_status": "active",
        "is_active": true, "created_at": "2026-10-31T00:00:00Z", "updated_at": "2026-10-31T00:00:00Z",
        "application_method": row["application_method"], "raw_payload": raw,
    });
    Programme {
        program: serde_json::from_value(program).expect("programme row"),
        detail: row["detail"].as_object().map(|o| Value::Object(o.clone())),
        admin_official_url: false,
        input_hash: "0".repeat(64),
    }
}

fn describe(outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Offline { reason } => json!({"outcome": "offline", "reason": reason}),
        Outcome::NotFound { reader, reason } => {
            json!({"outcome": "not_found", "reader": reader.as_str(), "reason": reason})
        }
        Outcome::Ambiguous { reader, source_url } => {
            json!({"outcome": "ambiguous", "reader": reader.as_str(), "source_url": source_url})
        }
        Outcome::Unreachable { reader, code } => {
            json!({"outcome": "unreachable", "reader": reader.as_str(), "reason": code})
        }
        Outcome::Passed(p) => json!({
            "outcome": "passed", "reader": p.reader.as_str(), "url": p.url, "label": p.label,
            "source_url": p.source_url, "depth": p.depth, "why": p.why, "access": p.access,
            "step": p.step, "final_url": p.final_url, "source_hash": p.source_hash,
            "content_hash": p.content_hash,
        }),
        Outcome::Failed(f) => json!({
            "outcome": "rejected", "reader": f.reader.as_str(), "url": f.url, "label": f.label,
            "source_url": f.source_url, "depth": f.depth, "why": f.why, "reason": f.code,
        }),
    }
}

/// Both passes' result as the job would store it after the second check.
fn verdict(first: &Outcome, second: &Outcome) -> &'static str {
    match (first, second) {
        (Outcome::Passed(a), Outcome::Passed(b))
            if a.source_hash == b.source_hash && a.content_hash == b.content_hash =>
        {
            "verified"
        }
        (Outcome::Passed(_), _) | (_, Outcome::Passed(_)) => "unstable",
        (_, Outcome::Offline { .. }) => "offline",
        (_, Outcome::Failed(_)) => "rejected",
        (_, Outcome::Ambiguous { .. }) => "ambiguous",
        (_, Outcome::Unreachable { .. }) => "unreachable",
        (_, Outcome::NotFound { .. }) => "not_found",
    }
}

async fn pass(
    pool: &sqlx::PgPool,
    judge: Option<&Judge>,
    programmes: &[Programme],
) -> Vec<Outcome> {
    let approved: Vec<String> = std::env::var("APPLY_LINKS_SAMPLE_APPROVED")
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let session = Session::new(pool.clone(), crate::sources::http::public_api_user_agent())
        .with_approved(approved);
    let mut lanes: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, p) in programmes.iter().enumerate() {
        let host = p
            .program
            .official_url
            .as_deref()
            .map(super::host_of)
            .unwrap_or_default();
        lanes.entry(host).or_default().push(i);
    }
    let mut out: Vec<Option<Outcome>> = vec![None; programmes.len()];
    let results = futures_util::stream::iter(lanes.into_values())
        .map(|indices| {
            let session = session.clone();
            async move {
                let mut done = vec![];
                for i in indices {
                    let outcome = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        evaluate(&session, judge, &programmes[i]),
                    )
                    .await
                    .unwrap_or_else(|_| Outcome::Unreachable {
                        reader: super::readers::reader_for(&programmes[i].inputs()),
                        code: "programme_timeout".into(),
                    });
                    done.push((i, outcome));
                }
                done
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    for (i, outcome) in results.into_iter().flatten() {
        out[i] = Some(outcome);
    }
    out.into_iter()
        .map(|o| o.expect("every programme"))
        .collect()
}

#[tokio::test]
#[ignore = "needs APPLY_LINKS_SAMPLE and network access"]
async fn apply_links_sample() {
    let (Ok(input), Ok(output)) = (
        std::env::var("APPLY_LINKS_SAMPLE"),
        std::env::var("APPLY_LINKS_SAMPLE_OUT"),
    ) else {
        return;
    };
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../infra/migrations")
        .run(&pool)
        .await
        .unwrap();
    let judge = Judge::from_env().expect("judge config");
    let rows: Vec<Value> = std::fs::read_to_string(&input)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let programmes: Vec<Programme> = rows.iter().map(programme).collect();
    let started = std::time::Instant::now();
    let first = pass(&pool, judge.as_ref(), &programmes).await;
    let first_secs = started.elapsed().as_secs();
    let gap: u64 = std::env::var("APPLY_LINKS_SAMPLE_GAP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    tokio::time::sleep(std::time::Duration::from_secs(gap)).await;
    let second = pass(&pool, judge.as_ref(), &programmes).await;
    let mut lines = String::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for ((row, a), b) in rows.iter().zip(&first).zip(&second) {
        let v = verdict(a, b);
        *counts
            .entry(format!("{} {v}", row["stratum"].as_str().unwrap_or("?")))
            .or_default() += 1;
        lines.push_str(
            &json!({
                "id": row["id"], "stratum": row["stratum"], "title": row["title"],
                "official_url": row["official_url"], "method": row["application_method"],
                "verdict": v, "first": describe(a), "second": describe(b),
            })
            .to_string(),
        );
        lines.push('\n');
    }
    std::fs::write(&output, lines).unwrap();
    println!(
        "first pass {first_secs}s, total {}s",
        started.elapsed().as_secs()
    );
    for (key, n) in counts {
        println!("{n:>4}  {key}");
    }
}
