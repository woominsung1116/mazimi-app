//! The "신청" deep-link resolver.
//!
//! Every programme keeps all its link candidates (`program_link_candidates`):
//! the source's URL fields in preference order, URLs written in its method
//! or deadline text, the portal detail page, and application links found on
//! its notice page. The worker checks them politely, classifies each with
//! the audit's rules ([`classify`]) under the origin and liveness policy
//! ([`resolve`]), and stores which one the programme shows
//! (`program_link_selections`). The API reads that selection; it never
//! fetches anything.
//!
//! Upkeep without a person: content changes re-derive and re-check a
//! programme's links; selected links are re-checked weekly (daily when the
//! window opens within 14 days), other candidates monthly; two hard
//! failures a day apart break a link and the next candidate takes over; a
//! broken link that passes twice comes back; soft failures (timeouts, TLS
//! errors, refused connections: kosaf, HUG, molit and kepco time out from
//! data centres) are recorded but never demote or break a link. Daily
//! self-metrics mark the job degraded when the share of application or
//! notice links drops by more than 2 points or more than 1% of programmes
//! show a link whose last check was a hard failure ([`metrics`]).
pub mod candidates;
pub mod check;
pub mod classify;
#[cfg(test)]
mod db_tests;
pub mod metrics;
pub mod page;
#[cfg(test)]
mod replay_tests;
pub mod repository;
pub mod resolve;
pub mod vocabulary;

use futures_util::StreamExt;
use sqlx::PgPool;
use std::{collections::BTreeMap, time::Duration};

/// The hourly job has a 25 minute hard cap; checks stop well before it.
const RUN_BUDGET: Duration = Duration::from_secs(20 * 60);
/// One URL's check (robots, redirects, pacing included) never takes longer.
const CHECK_LIMIT: Duration = Duration::from_secs(90);
/// Re-selections a stopped run left undone, taken per run.
const RESELECT_LIMIT: i64 = 500;

#[derive(Debug, Default, serde::Serialize)]
pub struct Summary {
    pub sync: repository::SyncSummary,
    pub reselected: usize,
    pub unrecorded_urls: usize,
    pub checked_urls: usize,
    pub deferred_urls: usize,
    pub results: repository::ApplySummary,
}

pub struct Job {
    sync_limit: i64,
    max_checks: i64,
}

impl Job {
    /// `None` unless `LINK_RESOLVER_ENABLED` is `true` or `1`.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !matches!(
            std::env::var("LINK_RESOLVER_ENABLED")
                .unwrap_or_default()
                .as_str(),
            "true" | "1"
        ) {
            return Ok(None);
        }
        fn bound(key: &str, default: i64, max: i64) -> anyhow::Result<i64> {
            let value = std::env::var(key)
                .ok()
                .map(|v| v.parse::<i64>())
                .transpose()?
                .unwrap_or(default);
            anyhow::ensure!((1..=max).contains(&value), "{key} out of bounds");
            Ok(value)
        }
        Ok(Some(Self {
            sync_limit: bound("LINK_RESOLVER_SYNC_LIMIT", 600, 5_000)?,
            max_checks: bound("LINK_RESOLVER_MAX_CHECKS", 300, 2_000)?,
        }))
    }

    pub fn new(sync_limit: i64, max_checks: i64) -> Self {
        Self {
            sync_limit,
            max_checks,
        }
    }

    /// One bounded pass: re-derive up to `sync_limit` programmes, then check
    /// up to `max_checks` due URLs, four hosts at a time.
    pub async fn run(&self, pool: &PgPool) -> anyhow::Result<Summary> {
        let deadline = tokio::time::Instant::now() + RUN_BUDGET;
        let approved = repository::approved_domains(pool).await?;
        let mut summary = Summary {
            reselected: repository::reselect_due(pool, RESELECT_LIMIT).await?,
            sync: repository::sync(pool, self.sync_limit, &approved).await?,
            ..Summary::default()
        };
        let due = repository::claim(pool, self.max_checks).await?;
        let mut by_host: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (url, host) in due {
            by_host.entry(host).or_default().push(url);
        }
        let user_agent = crate::sources::http::public_api_user_agent();
        let pacer = check::Pacer::default();
        let results = futures_util::stream::iter(by_host.into_values())
            .map(|urls| {
                let (pool, pacer, user_agent) = (pool.clone(), pacer.clone(), user_agent.clone());
                async move {
                    let mut local = repository::ApplySummary::default();
                    let (mut checked, mut deferred, mut unrecorded) = (0, 0, 0);
                    for url in urls {
                        if tokio::time::Instant::now() >= deadline {
                            // Still due: the next run takes it.
                            deferred += 1;
                            continue;
                        }
                        let observation = tokio::time::timeout(
                            CHECK_LIMIT,
                            check::check(&pool, &url, &user_agent, &pacer),
                        )
                        .await
                        .unwrap_or(classify::Observation::Error {
                            code: "timeout".into(),
                        });
                        checked += 1;
                        // One URL that cannot be recorded (a row the database
                        // refuses) never stops the lane or the run.
                        if let Err(error) =
                            repository::apply(&pool, &url, &observation, &mut local).await
                        {
                            unrecorded += 1;
                            tracing::warn!(error = %crate::error_safety::persisted_error(&error), "link check not recorded");
                            if let Err(error) = repository::postpone(&pool, &url).await {
                                tracing::warn!(error = %crate::error_safety::persisted_error(anyhow::Error::from(error)), "link check not postponed");
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>((checked, deferred, unrecorded, local))
                }
            })
            .buffer_unordered(check::HOSTS_AT_ONCE)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            let (checked, deferred, unrecorded, local) = result?;
            summary.checked_urls += checked;
            summary.unrecorded_urls += unrecorded;
            summary.deferred_urls += deferred;
            let r = &mut summary.results;
            r.passed += local.passed;
            r.blocked += local.blocked;
            r.soft += local.soft;
            r.hard += local.hard;
            r.newly_broken += local.newly_broken;
            r.restored += local.restored;
        }
        Ok(summary)
    }
}
