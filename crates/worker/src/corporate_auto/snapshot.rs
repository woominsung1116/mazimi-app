//! Real-page snapshots: the ground truth hide_only is measured on before it
//! is enabled (handoff §18).
//!
//! One fetch per source page of a published or audited-correct offer (the
//! 10-16 audit), through the same policy path the evidence job uses:
//! robots.txt for our agent, [`PinnedHttp`](super::http::PinnedHttp), the
//! per-domain spacing of a [`DomainSession`], bot walls and blocks skipped,
//! no login, no bypass. A page is stored only as it was served, with the
//! URL, the moment and its SHA-256.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::evidence::{audit, audit_fragments};
use super::http::PageHttp;
use super::policy::{DomainSession, Fetched, PageState};

/// One snapshot (or why there is none) in `index.json`. It carries no
/// earlier decision about the offer (audit verdict, visibility): the ground
/// truth labelled from these pages must not sit beside the inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub program_id: String,
    pub title: String,
    pub url: String,
    pub final_url: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub status: Option<u16>,
    /// `stored`, or why nothing was stored (`robots_disallow`, `bot_wall`, …).
    pub outcome: String,
    pub file: Option<String>,
    pub sha256: Option<String>,
    pub fragments: Vec<String>,
}

/// Outcomes that are a result about the page or its policy, not a failure
/// to read it. Allowlisted (review L-2): anything else, including an
/// outcome added later, fails the run (review M12).
const SNAPSHOT_RESULTS: &[&str] = &[
    "stored",
    "robots_disallow",
    "robots_forbidden",
    "robots_challenge",
    "robots_html",
    "robots_not_text",
    "page_bot_wall",
    "botwall",
    "gone",
    "unsupported",
    "request_budget",
    "redirect_without_location",
];

/// The page could not be read (network, server, robots unreachable, rate
/// limit, decoding, or any outcome not allowlisted as a result).
pub fn read_failure(outcome: &str) -> bool {
    !(SNAPSHOT_RESULTS.contains(&outcome) || outcome.starts_with("cross_host_redirect: "))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A page to snapshot: an offer, its URL and the fragments it is read with.
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotTarget {
    pub program_id: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub fragments: Vec<String>,
}

/// Fetches every page of a published or audited-correct offer once and
/// writes `<dir>/<id>.html` and `<dir>/index.json`. Fails closed: when any
/// page could not be read ([`read_failure`]) nothing is written.
pub async fn run(http: &dyn PageHttp, dir: &Path) -> Result<Vec<SnapshotEntry>> {
    let targets = audit()
        .items
        .iter()
        .filter(|item| item.visible || item.verdict == "CORRECT")
        .filter_map(|item| {
            Some(SnapshotTarget {
                program_id: item.id.to_string(),
                title: item.title.clone(),
                url: item.official_url.clone()?,
                fragments: audit_fragments(item).unwrap_or_default(),
            })
        })
        .collect();
    run_targets(http, dir, targets).await
}

/// [`run`] for a given list (the known-ended set, `--targets FILE`).
pub async fn run_targets(
    http: &dyn PageHttp,
    dir: &Path,
    targets: Vec<SnapshotTarget>,
) -> Result<Vec<SnapshotEntry>> {
    run_targets_spaced(http, dir, targets, None).await
}

/// [`run_targets`]; `test_spacing` replaces the production request spacing
/// (tests only).
async fn run_targets_spaced(
    http: &dyn PageHttp,
    dir: &Path,
    targets: Vec<SnapshotTarget>,
    test_spacing: Option<std::time::Duration>,
) -> Result<Vec<SnapshotEntry>> {
    let mut entries = Vec::new();
    let mut pages: Vec<(String, String)> = Vec::new();
    let mut sessions: std::collections::HashMap<String, DomainSession<'_>> = Default::default();
    for target in targets {
        let url = target.url.as_str();
        let fragments = target.fragments.clone();
        let mut entry = SnapshotEntry {
            program_id: target.program_id.clone(),
            title: target.title.clone(),
            url: url.to_string(),
            final_url: None,
            fetched_at: Utc::now(),
            status: None,
            outcome: String::new(),
            file: None,
            sha256: None,
            fragments,
        };
        let Ok(mut current) = Url::parse(url) else {
            entry.outcome = "invalid_url".into();
            entries.push(entry);
            continue;
        };
        for _hop in 0..3 {
            let host = current.host_str().unwrap_or("").to_string();
            let session = sessions.entry(host).or_insert_with(|| {
                let session = DomainSession::new(http, None, None, false, 0, 6);
                match test_spacing {
                    Some(spacing) => session.with_test_spacing(spacing),
                    None => session,
                }
            });
            let (fetched, state) = session.page(&current, false).await;
            entry.fetched_at = Utc::now();
            entry.final_url = Some(current.to_string());
            if let Fetched::Reply(reply) = &fetched {
                entry.status = Some(reply.status);
            }
            match state {
                PageState::Html { .. } => {
                    let Fetched::Reply(reply) = fetched else {
                        entry.outcome = "missing_reply".into();
                        break;
                    };
                    let html = match crate::sources::benefit_pages::decode_page(
                        &reply.body,
                        &reply.content_type,
                    ) {
                        Ok(html) => html,
                        Err(error) => {
                            entry.outcome = format!("undecodable: {error}");
                            break;
                        }
                    };
                    let name = format!(
                        "{}.html",
                        entry.program_id.chars().take(8).collect::<String>()
                    );
                    entry.sha256 = Some(hex(&Sha256::digest(html.as_bytes())));
                    pages.push((name.clone(), html));
                    entry.file = Some(name);
                    entry.outcome = "stored".into();
                    break;
                }
                PageState::Redirect { location, .. } => {
                    let Ok(next) = current.join(&location) else {
                        entry.outcome = "redirect_without_location".into();
                        break;
                    };
                    if next.host_str() != current.host_str() {
                        entry.outcome = format!("cross_host_redirect: {next}");
                        break;
                    }
                    current = next;
                    entry.outcome = "too_many_redirects".into();
                }
                other => {
                    entry.outcome = match &fetched {
                        Fetched::Stopped(reason) => (*reason).to_string(),
                        Fetched::Failed(error) => format!("failed: {error}"),
                        Fetched::BudgetExhausted => "request_budget".to_string(),
                        _ => format!("{other:?}")
                            .split(['(', ' ', '{'])
                            .next()
                            .unwrap_or("skipped")
                            .to_lowercase(),
                    };
                    break;
                }
            }
        }
        tracing::info!(url = %entry.url, outcome = %entry.outcome, "snapshot");
        entries.push(entry);
    }
    let failed: Vec<String> = entries
        .iter()
        .filter(|e| read_failure(&e.outcome))
        .map(|e| format!("{} {}", e.outcome, e.url))
        .collect();
    if !failed.is_empty() {
        anyhow::bail!(
            "{} pages could not be read; nothing written ({})",
            failed.len(),
            failed[..failed.len().min(5)].join("; ")
        );
    }
    let index = serde_json::to_string_pretty(&entries).context("index")?;
    pages.push(("index.json".into(), index));
    write_all(dir, &pages)?;
    Ok(entries)
}

/// Writes every file to a temporary name first and renames them only once
/// all are written, the index last, so a failed write leaves the earlier
/// snapshot as it was.
fn write_all(dir: &Path, files: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let temporary = |name: &str| dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = files.iter().try_for_each(|(name, body)| {
        std::fs::write(temporary(name), body.as_bytes())
            .with_context(|| format!("failed to write {name}"))
    });
    let renamed = written.and_then(|()| {
        files.iter().try_for_each(|(name, _)| {
            std::fs::rename(temporary(name), dir.join(name))
                .with_context(|| format!("failed to write {name}"))
        })
    });
    if renamed.is_err() {
        for (name, _) in files {
            let _ = std::fs::remove_file(temporary(name));
        }
    }
    renamed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corporate_auto::http::LoopbackHttp;

    const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";

    async fn serve() -> String {
        use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
        let app = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route(
                "/ok",
                get(|| async {
                    (
                        [("content-type", "text/html; charset=utf-8")],
                        "<html><body><p>학생 10% 할인</p></body></html>",
                    )
                }),
            )
            .route(
                "/down",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "down").into_response() }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    fn target(id: &str, url: String) -> SnapshotTarget {
        SnapshotTarget {
            program_id: id.into(),
            title: id.into(),
            url,
            fragments: vec![],
        }
    }

    /// Review M12: a page that could not be read fails the whole run (the
    /// CLI exits non-zero) and nothing is written: no page, no index. A
    /// clean run writes every page and the index.
    #[tokio::test]
    async fn a_failed_fetch_fails_the_run_and_writes_nothing() {
        let origin = serve().await;
        let dir = std::env::temp_dir().join(format!("snapshot-{}", uuid::Uuid::new_v4().simple()));
        let http = LoopbackHttp::new(UA);
        let spacing = Some(std::time::Duration::from_millis(5));
        let targets = vec![
            target("aaaaaaaa-ok", format!("{origin}/ok")),
            target("bbbbbbbb-down", format!("{origin}/down")),
        ];
        let failed = run_targets_spaced(&http, &dir, targets, spacing).await;
        assert!(failed.is_err(), "{failed:?}");
        let written: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(written.is_empty(), "{written:?}");

        let entries = run_targets_spaced(
            &http,
            &dir,
            vec![target("aaaaaaaa-ok", format!("{origin}/ok"))],
            spacing,
        )
        .await
        .unwrap();
        assert_eq!(entries[0].outcome, "stored");
        assert!(dir.join("aaaaaaaa.html").exists());
        assert!(dir.join("index.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_read_failures_fail_a_run() {
        for error in [
            "failed: timeout",
            "undecodable: bad charset",
            "invalid_url",
            "missing_reply",
            "transient",
            "robots_unreachable",
            "domain_stopped",
            "rate_limited",
            "ratelimited",
            "deferred",
            "too_many_redirects",
            "an_outcome_added_later",
        ] {
            assert!(read_failure(error), "{error}");
        }
        for decision in [
            "stored",
            "robots_disallow",
            "robots_forbidden",
            "botwall",
            "page_bot_wall",
            "gone",
            "unsupported",
            "request_budget",
            "cross_host_redirect: https://x.example/",
        ] {
            assert!(!read_failure(decision), "{decision}");
        }
    }
}
