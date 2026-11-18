mod alerts;
mod notifications;

use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use tokio_cron_scheduler::{Job, JobScheduler};

use mazimi_worker::data_maintenance;
use mazimi_worker::enrichment;
use mazimi_worker::operations;
use mazimi_worker::runtime;
use mazimi_worker::source_monitoring;
use mazimi_worker::sources::benefit_pages::BenefitPageFetcher;
use mazimi_worker::sources::official_seeds::OfficialSeedDiscoverer;

use crate::notifications::NotificationDispatcher;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let app_env = runtime::environment()?;
    runtime::init_tracing(&app_env);
    runtime::remove_worker_heartbeat().await;
    // An invalid corporate flag (an unknown CORPORATE_AUTO_HIDE_REASONS key,
    // an unknown mode, a missing contact URL) stops the worker here instead
    // of silently turning the corporate jobs off.
    mazimi_worker::corporate_auto::Flags::from_env()?;

    let pool = runtime::connect_worker_pool().await?;
    let scheduler_lease = runtime::SchedulerLease::acquire(&pool).await?;

    tracing::info!(
        service = "mazimi-worker",
        environment = %app_env,
        "worker connected to database and acquired singleton lease"
    );

    // 알림 채널 초기화 (env 없는 채널은 warn 후 skip)
    let notifier = std::sync::Arc::new(NotificationDispatcher::from_env(pool.clone()));

    let mut sched = JobScheduler::new().await?;

    // Refresh configured seed URLs every six hours so admin configuration
    // changes are picked up without a worker restart. The upsert is idempotent.
    sched
        .add(Job::new_async("0 5 */6 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match OfficialSeedDiscoverer::run(&pool).await {
                        Ok(_) => resolve_job_incident(&pool, "job:official_seed_discovery").await,
                        Err(error) => {
                            tracing::error!(error = %error, "official seed discovery failed");
                            open_job_incident(
                                &pool,
                                "job:official_seed_discovery",
                                operations::Severity::Error,
                                "official_seed_discovery",
                                "Official benefit seed discovery failed",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Health check every 60 seconds
    sched
        .add(Job::new_async("0 * * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match sqlx::query_scalar::<_, i32>("SELECT 1")
                        .fetch_one(&pool)
                        .await
                    {
                        Ok(_) => match runtime::write_worker_heartbeat().await {
                            Ok(()) => tracing::debug!("worker health check passed"),
                            Err(error) => {
                                tracing::error!(error = %error, "worker heartbeat update failed")
                            }
                        },
                        Err(error) => {
                            runtime::remove_worker_heartbeat().await;
                            tracing::error!(error = %error, "worker database health check failed");
                        }
                    }
                })
            }
        })?)
        .await?;

    // Durable state catches up after a reboot (at the end of
    // startup_catch_up) and retries individual failed sources hourly.
    sched
        .add(Job::new_async("0 10 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Err(error) = mazimi_worker::daily::run_due(&pool).await {
                        tracing::error!(error=%error,"daily collection reconciliation failed");
                    }
                })
            }
        })?)
        .await?;

    // A-09 AI extraction: 03:30-06:30 KST (18:30-21:30 UTC) hourly, away from
    // user traffic. Off unless EXTRACTION_AI_ENABLED. The daily-job lease
    // allows one successful batch per Korean day and up to 3 attempts an hour
    // apart, so a failed night retries inside this window (panic isolation,
    // 30-minute cap). Results are review candidates only.
    sched
        .add(Job::new_async("0 30 18-21 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Err(error) = mazimi_worker::daily::run_ai_extraction(&pool).await {
                        tracing::error!(error = %error, "ai extraction job bookkeeping failed");
                    }
                })
            }
        })?)
        .await?;

    // Programs whose source text changed on ingestion are queued by a
    // database trigger and drained hourly in small batches (default 4), so a
    // changed notice is re-extracted the same day instead of waiting for the
    // nightly sweep. Off unless EXTRACTION_AI_ENABLED.
    sched
        .add(Job::new_async("0 45 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Err(error) = mazimi_worker::daily::run_ai_extraction_queue(&pool).await {
                        tracing::error!(error = %mazimi_worker::error_safety::persisted_error(&error), "ai extraction queue drain failed");
                    }
                })
            }
        })?)
        .await?;

    // Compliance-approved public benefit pages every six hours. This job is
    // independently opt-in and only claims candidates whose domain approval,
    // path scope and robots review are all current. Results remain admin-only
    // until a person approves and publishes a normalized benefit.
    sched
        .add(Job::new_async("0 15 */6 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match BenefitPageFetcher::from_env() {
                        Ok(Some(fetcher)) => match fetcher.run(&pool).await {
                            Ok(summary) => {
                                tracing::info!(
                                    claimed = summary.claimed,
                                    review = summary.ready_for_review,
                                    failed = summary.failed,
                                    blocked = summary.blocked,
                                    "approved benefit-page fetch complete"
                                );
                                resolve_job_incident(&pool, "job:benefit_page_fetch_failed").await;
                                if summary.failed > 0 {
                                    open_job_incident(
                                        &pool,
                                        "job:benefit_page_fetch_partial",
                                        operations::Severity::Warning,
                                        "benefit_page_fetch_partial",
                                        "Approved benefit-page collection completed with failures",
                                        serde_json::json!({
                                            "claimed": summary.claimed,
                                            "failed": summary.failed
                                        }),
                                    )
                                    .await;
                                } else {
                                    resolve_job_incident(&pool, "job:benefit_page_fetch_partial")
                                        .await;
                                }
                            }
                            Err(error) => {
                                tracing::error!(
                                    error = %error,
                                    "approved benefit-page fetch run failed"
                                );
                                open_job_incident(
                                    &pool,
                                    "job:benefit_page_fetch_failed",
                                    operations::Severity::Error,
                                    "benefit_page_fetch_failed",
                                    "Approved benefit-page collection failed",
                                    serde_json::json!({}),
                                )
                                .await;
                            }
                        },
                        Ok(None) => tracing::debug!(
                            "BENEFIT_CRAWLER_ENABLED is false — approved page fetch skipped"
                        ),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                "invalid benefit-page crawler configuration"
                            );
                            open_job_incident(
                                &pool,
                                "job:benefit_page_fetch_failed",
                                operations::Severity::Error,
                                "benefit_page_fetch_configuration",
                                "Benefit-page crawler configuration is invalid",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Optional structured extraction runs after approved-page collection.
    // It is disabled by default and only produces admin-review drafts; no LLM
    // result can create or publish a program.
    sched
        .add(Job::new_async("0 25 */6 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match enrichment::run_pending_from_env(&pool).await {
                        Ok(Some(summary)) => {
                            tracing::info!(
                                selected = summary.selected,
                                succeeded = summary.succeeded,
                                cache_hits = summary.cache_hits,
                                rejected = summary.rejected,
                                failed = summary.failed,
                                "candidate draft enrichment complete"
                            );
                            resolve_job_incident(&pool, "job:benefit_enrichment_failed").await;
                            if summary.failed > 0 {
                                open_job_incident(
                                    &pool,
                                    "job:benefit_enrichment_partial",
                                    operations::Severity::Warning,
                                    "benefit_enrichment_partial",
                                    "Candidate enrichment completed with provider failures",
                                    serde_json::json!({
                                        "selected": summary.selected,
                                        "failed": summary.failed
                                    }),
                                )
                                .await;
                            } else {
                                resolve_job_incident(&pool, "job:benefit_enrichment_partial").await;
                            }
                        }
                        Ok(None) => tracing::debug!(
                            "BENEFIT_LLM_ENABLED is false — candidate enrichment skipped"
                        ),
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                "candidate draft enrichment failed"
                            );
                            open_job_incident(
                                &pool,
                                "job:benefit_enrichment_failed",
                                operations::Severity::Error,
                                "benefit_enrichment_failed",
                                "Candidate enrichment job failed",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    sched
        .add(Job::new_async("0 35 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // External pages feed the fetcher; a parser panic must not take
                    // the scheduler down with it, so it is contained and recorded
                    // like any other failure of this job.
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(1500),
                        AssertUnwindSafe(async {
                            if let Some(job) =
                                mazimi_worker::application_resources::Job::from_env()?
                            {
                                let summary = job.run(&pool).await?;
                                tracing::info!(?summary, "application resources checked");
                            }
                            Ok::<_, anyhow::Error>(())
                        })
                        .catch_unwind(),
                    )
                    .await;
                    let failure = match result {
                        Ok(Ok(Ok(()))) => None,
                        Ok(Ok(Err(error))) => {
                            Some(mazimi_worker::error_safety::persisted_error(&error))
                        }
                        Ok(Err(_panic)) => Some("job panicked".to_string()),
                        Err(_) => Some("job exceeded 25 minute limit".to_string()),
                    };
                    match failure {
                        None => resolve_job_incident(&pool, "job:application_resources").await,
                        Some(error) => {
                            tracing::error!(error = %error, "application resource job failed");
                            open_job_incident(
                                &pool,
                                "job:application_resources",
                                operations::Severity::Error,
                                "application_resources",
                                "Application resource checks failed",
                                serde_json::json!({ "error": error }),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // The "신청" link resolver: candidate sync and polite link checks,
    // hourly at :40, bounded per run (LINK_RESOLVER_SYNC_LIMIT programmes,
    // LINK_RESOLVER_MAX_CHECKS URLs, 20-minute budget, 25-minute cap). A
    // cron job only: it never runs on the start path, and it waits for the
    // first heartbeat. Off unless LINK_RESOLVER_ENABLED.
    sched
        .add(Job::new_async("0 40 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if !std::path::Path::new(runtime::WORKER_HEARTBEAT_PATH).exists() {
                        tracing::info!("link resolver waits for the worker heartbeat");
                        return;
                    }
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(1500),
                        AssertUnwindSafe(async {
                            let pass = async {
                                if let Some(job) = mazimi_worker::link_resolver::Job::from_env()? {
                                    let summary = job.run(&pool).await?;
                                    tracing::info!(?summary, "link resolver pass complete");
                                }
                                Ok::<_, anyhow::Error>(())
                            }
                            .await;
                            // The self-metrics run even when the pass failed:
                            // that is when the degraded signal matters.
                            let metrics = mazimi_worker::daily::run_link_metrics(&pool).await;
                            pass.and(metrics)
                        })
                        .catch_unwind(),
                    )
                    .await;
                    let failure = match result {
                        Ok(Ok(Ok(()))) => None,
                        Ok(Ok(Err(error))) => {
                            Some(mazimi_worker::error_safety::persisted_error(&error))
                        }
                        Ok(Err(_panic)) => Some("job panicked".to_string()),
                        Err(_) => Some("job exceeded 25 minute limit".to_string()),
                    };
                    match failure {
                        None => resolve_job_incident(&pool, "job:link_resolver").await,
                        Some(error) => {
                            tracing::error!(error = %error, "link resolver job failed");
                            open_job_incident(
                                &pool,
                                "job:link_resolver",
                                operations::Severity::Error,
                                "link_resolver",
                                "Application link checks failed",
                                serde_json::json!({ "error": error }),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Automatic 신청 entry points (application_resources::apply_entry):
    // hourly at :32, bounded per run (APPLY_LINKS_MAX_PROGRAMMES, 20-minute
    // budget, 25-minute cap). A cron job only; it waits for the first
    // heartbeat. Off unless APPLY_LINKS_ENABLED.
    sched
        .add(Job::new_async("0 32 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if !std::path::Path::new(runtime::WORKER_HEARTBEAT_PATH).exists() {
                        return;
                    }
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(1500),
                        AssertUnwindSafe(async {
                            if let Some(job) =
                                mazimi_worker::application_resources::apply_entry::Job::from_env()?
                            {
                                let summary = job.run(&pool).await?;
                                tracing::info!(?summary, "apply link pass complete");
                            }
                            Ok::<_, anyhow::Error>(())
                        })
                        .catch_unwind(),
                    )
                    .await;
                    let failure = match result {
                        Ok(Ok(Ok(()))) => None,
                        Ok(Ok(Err(error))) => {
                            Some(mazimi_worker::error_safety::persisted_error(&error))
                        }
                        Ok(Err(_panic)) => Some("job panicked".to_string()),
                        Err(_) => Some("job exceeded 25 minute limit".to_string()),
                    };
                    match failure {
                        None => resolve_job_incident(&pool, "job:apply_links").await,
                        Some(error) => {
                            tracing::error!(error = %error, "apply link job failed");
                            open_job_incident(
                                &pool,
                                "job:apply_links",
                                operations::Severity::Error,
                                "apply_links",
                                "Automatic application link checks failed",
                                serde_json::json!({ "error": error }),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Give the approved-page fetcher a 30-minute head start. Sources that are
    // still beyond their review deadline plus grace period are then placed on
    // a reversible publication hold, regardless of why they were not checked.
    sched
        .add(Job::new_async("0 45 */6 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match source_monitoring::freshness_policy_from_env() {
                        Ok((grace, limit)) => {
                            match source_monitoring::run_source_maintenance(&pool, grace, limit)
                                .await
                            {
                                Ok(summary) => {
                                    tracing::info!(
                                        schedules_reconciled = summary.schedules_reconciled,
                                        unmonitored_programs_held =
                                            summary.unmonitored_programs_held,
                                        overdue_candidates_held = summary.overdue_candidates_held,
                                        overdue_programs_held = summary.overdue_programs_held,
                                        "source maintenance sweep complete"
                                    );
                                    resolve_job_incident(&pool, "job:source_freshness").await;
                                }
                                Err(error) => {
                                    tracing::error!(
                                        error = %error,
                                        "source freshness sweep failed"
                                    );
                                    open_job_incident(
                                        &pool,
                                        "job:source_freshness",
                                        operations::Severity::Error,
                                        "source_freshness_sweep",
                                        "Source freshness sweep failed",
                                        serde_json::json!({}),
                                    )
                                    .await;
                                }
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                "invalid source freshness configuration"
                            );
                            open_job_incident(
                                &pool,
                                "job:source_freshness",
                                operations::Severity::Error,
                                "source_freshness_configuration",
                                "Source freshness worker configuration is invalid",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Keep duplicate and quality queues current after the source freshness
    // sweep. Explicitly expired benefits are hidden; suspected duplicates are
    // review-only and are never merged by this job.
    sched
        .add(Job::new_async("0 50 */6 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match data_maintenance::run(&pool).await {
                        Ok(summary) => {
                            tracing::info!(
                                duplicate_candidates_upserted = summary.duplicate_candidates_upserted,
                                quality_issues_observed = summary.quality_issues_observed,
                                programs_expired = summary.programs_expired,
                                "corporate benefit data maintenance complete"
                            );
                            resolve_job_incident(&pool, "job:corporate_benefit_maintenance").await;
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "corporate benefit data maintenance failed");
                            open_job_incident(
                                &pool,
                                "job:corporate_benefit_maintenance",
                                operations::Severity::Error,
                                "corporate_benefit_maintenance",
                                "Corporate benefit data maintenance failed",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;

    // Programme category backfill (migration 20261023000002): bounded runs
    // of 500-row batches every five minutes, never on the startup path. New
    // and changed rows are kept current by a trigger.
    sched
        .add(Job::new_async("40 */5 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Some(run) = mazimi_worker::categories::refresh_scheduled(&pool).await {
                        if run.updated > 0 || run.remaining > 0 {
                            tracing::info!(
                                updated = run.updated,
                                remaining = run.remaining,
                                "programme categories refreshed"
                            );
                        }
                    }
                })
            }
        })?)
        .await?;

    // 기업혜택 browse chips right after a deploy: one guarded refresh, spawned
    // so startup never waits on it; a no-op once every row has a chip.
    tokio::spawn({
        let pool = pool.clone();
        async move {
            if let Some(run) = mazimi_worker::categories::refresh_browse_on_start(&pool).await {
                tracing::info!(
                    updated = run.updated,
                    remaining = run.remaining,
                    "기업혜택 browse chips filled at start"
                );
            }
        }
    });

    // 기업혜택 browse chips (migration 20261026000301): the sweep for rows
    // the write paths did not store, the post-migration backfill and rule
    // changes. Every five minutes, never on the startup path.
    sched
        .add(Job::new_async("50 */5 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Some(run) =
                        mazimi_worker::categories::refresh_browse_scheduled(&pool).await
                    {
                        if run.updated > 0 || run.cleared > 0 || run.remaining > 0 {
                            tracing::info!(
                                updated = run.updated,
                                cleared = run.cleared,
                                remaining = run.remaining,
                                "기업혜택 browse chips refreshed"
                            );
                        }
                    }
                })
            }
        })?)
        .await?;

    // rule_v1 text-rule measurement (nothing serves it). Each run is bounded
    // (1,000 rows or 60 s) and goes on where the last stopped, so a full
    // backfill of the catalogue spreads over many ticks. Never fatal: a
    // failure opens job:text_rules_refresh and stored rows stay.
    sched
        .add(Job::new_async("20 */10 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Some(rules) =
                        mazimi_worker::text_rules::refresh::refresh_monitored(&pool).await
                    {
                        tracing::info!(
                            pending = rules.pending,
                            examined = rules.examined,
                            written = rules.written,
                            failed = rules.failed,
                            parked = rules.parked,
                            remaining = rules.remaining(),
                            skipped_unchanged = rules.skipped_unchanged,
                            "rule_v1 text-rule measurement refreshed"
                        );
                    }
                })
            }
        })?)
        .await?;

    // Durable operator incidents are always written to PostgreSQL. Delivery is
    // independently opt-in: without OPS_ALERT_WEBHOOK_URL or
    // OPS_ALERT_EMAIL_TO this job is a no-op.
    sched
        .add(Job::new_async("30 */5 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    match operations::dispatch_pending_from_env(&pool).await {
                        Ok(Some(summary)) if summary.selected > 0 => tracing::info!(
                            selected = summary.selected,
                            delivered = summary.delivered,
                            failed = summary.failed,
                            "operator alert delivery complete"
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::error!(
                            error = %error,
                            "operator alert delivery failed"
                        ),
                    }
                    // Email is independent of the webhook (OPS_ALERT_EMAIL_TO).
                    match operations::dispatch_email_pending_from_env(&pool).await {
                        Ok(Some(summary)) if summary.selected > 0 => tracing::info!(
                            selected = summary.selected,
                            delivered = summary.delivered,
                            failed = summary.failed,
                            "operator alert email complete"
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::error!(
                            error = %error,
                            "operator alert email failed"
                        ),
                    }
                })
            }
        })?)
        .await?;

    // Storage upkeep 04:30 KST (19:30 UTC), retried at 05:30 and 06:30 KST
    // by the daily-job lease. Not part of the startup catch-up.
    sched
        .add(Job::new_async("0 30 19-21 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Err(error) = mazimi_worker::daily::run_storage(&pool).await {
                        tracing::error!(error = %error, "storage upkeep bookkeeping failed");
                    }
                })
            }
        })?)
        .await?;

    // Daily operator digest at 09:00 KST (00:00 UTC), sent even when healthy.
    // The hourly reconciliation catches up if the worker was down at 09:00.
    sched
        .add(Job::new_async("0 0 0 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    if let Err(error) = mazimi_worker::daily::run_ops_digest(&pool).await {
                        tracing::error!(error = %error, "operator digest bookkeeping failed");
                    }
                })
            }
        })?)
        .await?;

    // Expo push receipt check every 15 minutes
    sched
        .add(Job::new_async("0 */15 * * * *", {
            let notifier = notifier.clone();
            move |_uuid, _l| {
                let notifier = notifier.clone();
                Box::pin(async move {
                    notifier.check_pending_receipts().await;
                })
            }
        })?)
        .await?;

    // Deadline alerts every hour (D-7/D-3/D-1)
    sched
        .add(Job::new_async("0 0 * * * *", {
            let pool = pool.clone();
            let notifier = notifier.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                let notifier = notifier.clone();
                Box::pin(async move {
                    match alerts::generate_deadline_alerts(&pool, &notifier).await {
                        Ok(_) => resolve_job_incident(&pool, "job:deadline_alerts").await,
                        Err(error) => {
                            tracing::error!(error = %error, "deadline alert generation failed");
                            open_job_incident(
                                &pool,
                                "job:deadline_alerts",
                                operations::Severity::Error,
                                "deadline_alert_generation",
                                "User deadline-alert generation failed",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                })
            }
        })?)
        .await?;
    // AI agent: run-finished pushes (outbox) every 5 minutes; retention
    // purge hourly (always on); proactive messages hourly only when
    // ASSISTANT_AGENT_PROACTIVE_ENABLED and only to opted-in users.
    sched
        .add(Job::new_async("0 */5 * * * *", {
            let pool = pool.clone();
            let notifier = notifier.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                let notifier = notifier.clone();
                Box::pin(async move {
                    match mazimi_worker::assistant_agent::drain_outbox(
                        &pool,
                        chrono::Utc::now(),
                        200,
                    )
                    .await
                    {
                        Ok(messages) => send_agent_messages(&notifier, messages).await,
                        Err(error) => tracing::error!(error = %error, "agent outbox drain failed"),
                    }
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 20 * * * *", {
            let pool = pool.clone();
            let notifier = notifier.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                let notifier = notifier.clone();
                Box::pin(async move {
                    use mazimi_worker::assistant_agent as agent;
                    match agent::purge(&pool, agent::retention_days()).await {
                        Ok(summary) => tracing::info!(?summary, "agent retention purge complete"),
                        Err(error) => {
                            tracing::error!(error = %error, "agent retention purge failed")
                        }
                    }
                    if !agent::proactive_enabled() {
                        return;
                    }
                    let now = chrono::Utc::now();
                    for (job, result) in [
                        ("weekly_briefing", agent::weekly_briefings(&pool, now).await),
                        ("change_impact", agent::change_impacts(&pool, now).await),
                        ("resume", agent::resume_suggestions(&pool, now).await),
                    ] {
                        match result {
                            Ok(messages) => send_agent_messages(&notifier, messages).await,
                            Err(error) => {
                                tracing::error!(job, error = %error, "agent proactive job failed")
                            }
                        }
                    }
                })
            }
        })?)
        .await?;
    // Corporate benefits without humans in the loop (P0 census, P1 evidence
    // re-verification, daily health). Each job returns at once while its
    // flag is off; see corporate_auto::Flags.
    sched
        .add(Job::new_async("0 55 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    mazimi_worker::corporate_auto::evidence::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    // Sources the evidence job queues for fresh reading have one consumer,
    // intake's held-source lane; the separate fresh-extraction job is
    // retired (corporate_auto::evidence::Exit).
    sched
        .add(Job::new_async("0 25 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // P2 intake: candidate → programme with no person in
                    // the loop. CORPORATE_INTAKE_MODE (default shadow: it
                    // records decisions and writes no programme).
                    mazimi_worker::corporate_auto::intake::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 35 18 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // 03:35 KST: the weekly re-extraction audit, a bounded
                    // slice a day (measurement only); off unless
                    // CORPORATE_WEEKLY_AUDIT_ENABLED.
                    mazimi_worker::corporate_auto::weekly_audit::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 25 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // Publisher opt-out proofs (.well-known); off with the intake flag.
                    mazimi_worker::corporate_auto::optout::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 40 17 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // 02:40 KST, off-peak; the job itself runs at most every 6 days.
                    mazimi_worker::corporate_auto::census::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 40 * * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // Corporate doors: each due door at most once a day;
                    // returns at once while CORPORATE_DOORS_MODE is off.
                    mazimi_worker::corporate_auto::doors::job::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched
        .add(Job::new_async("0 50 14 * * *", {
            let pool = pool.clone();
            move |_uuid, _l| {
                let pool = pool.clone();
                Box::pin(async move {
                    // 23:50 KST.
                    mazimi_worker::corporate_auto::health::run_scheduled(&pool).await;
                })
            }
        })?)
        .await?;
    sched.start().await?;

    // The container is ready only after the scheduler is running and the DB
    // responded successfully. The periodic health job refreshes this file.
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await?;
    runtime::write_worker_heartbeat().await?;

    tracing::info!(service = "mazimi-worker", "worker scheduler started");

    tokio::spawn(startup_catch_up(pool.clone()));

    runtime::shutdown_signal().await;
    tracing::info!(service = "mazimi-worker", "worker shutdown signal received");
    runtime::remove_worker_heartbeat().await;
    sched.shutdown().await?;
    scheduler_lease.release().await;
    if tokio::time::timeout(std::time::Duration::from_secs(30), pool.close())
        .await
        .is_err()
    {
        tracing::warn!("database pool did not close within the worker shutdown deadline");
    }
    tracing::info!(service = "mazimi-worker", "worker shutdown complete");

    Ok(())
}

/// Push agent messages through the shared dispatcher. Push only: no Kakao
/// template exists for agent messages, so `phone` stays empty.
async fn send_agent_messages(
    notifier: &NotificationDispatcher,
    messages: Vec<mazimi_worker::assistant_agent::Outgoing>,
) {
    for message in messages {
        let payload = notifications::NotificationPayload {
            user_id: message.user_id,
            phone: None,
            expo_tokens: message.expo_tokens.clone(),
            program_id: message.program_id,
            template_code: "AGENT".into(),
            title: message.title.clone(),
            body: message.body.clone(),
            template_vars: std::collections::HashMap::new(),
        };
        if let Err(error) = notifier.send(&payload).await {
            tracing::warn!(error = %error, message = %mazimi_worker::assistant_agent::describe(&message), "agent push failed");
        }
    }
}

async fn open_job_incident(
    pool: &sqlx::PgPool,
    fingerprint: &str,
    severity: operations::Severity,
    event_type: &str,
    message: &str,
    details: serde_json::Value,
) {
    if let Err(error) = operations::record_event(
        pool,
        &operations::EventCommand {
            fingerprint,
            severity,
            component: "scheduler",
            event_type,
            subject_type: None,
            subject_id: None,
            message,
            details,
        },
    )
    .await
    {
        tracing::error!(error = %error, fingerprint, "failed to persist scheduler incident");
    }
}

async fn resolve_job_incident(pool: &sqlx::PgPool, fingerprint: &str) {
    if let Err(error) =
        operations::resolve_event(pool, fingerprint, None, Some("scheduled job recovered")).await
    {
        tracing::error!(error = %error, fingerprint, "failed to resolve scheduler incident");
    }
}

/// The startup catch-up: seed discovery, the source freshness sweep, data
/// maintenance and the due daily jobs, in that order. Spawned after the heartbeat so no
/// startup step can hold back readiness, however long it takes on the
/// production catalogue (incident 2026-10-22). Each step is also a
/// scheduled job, so a failure here only waits for the next tick.
async fn startup_catch_up(pool: sqlx::PgPool) {
    // Configured official URLs are safe to enqueue without fetching. Running
    // this at startup gives a new environment useful review candidates even
    // before licensed-search credentials are provisioned.
    match OfficialSeedDiscoverer::run(&pool).await {
        Ok(_) => resolve_job_incident(&pool, "job:official_seed_discovery").await,
        Err(error) => {
            tracing::error!(error = %error, "initial official seed discovery failed");
            open_job_incident(
                &pool,
                "job:official_seed_discovery",
                operations::Severity::Error,
                "official_seed_discovery",
                "Official benefit seed discovery failed",
                serde_json::json!({}),
            )
            .await;
        }
    }

    // Source freshness is independent from direct crawling. Even when legal
    // approval is absent and the page fetcher is disabled, an overdue public
    // benefit is hidden after the configured grace period instead of being
    // served indefinitely with stale terms.
    match source_monitoring::freshness_policy_from_env() {
        Ok((grace, limit)) => {
            match source_monitoring::run_source_maintenance(&pool, grace, limit).await {
                Ok(summary) => {
                    tracing::info!(
                        schedules_reconciled = summary.schedules_reconciled,
                        unmonitored_programs_held = summary.unmonitored_programs_held,
                        overdue_candidates_held = summary.overdue_candidates_held,
                        overdue_programs_held = summary.overdue_programs_held,
                        "initial source maintenance sweep complete"
                    );
                    resolve_job_incident(&pool, "job:source_freshness").await;
                }
                Err(error) => {
                    tracing::error!(error = %error, "initial source freshness sweep failed");
                    open_job_incident(
                        &pool,
                        "job:source_freshness",
                        operations::Severity::Error,
                        "source_freshness_sweep",
                        "Source freshness sweep failed",
                        serde_json::json!({}),
                    )
                    .await;
                }
            }
        }
        Err(error) => {
            tracing::error!(error = %error, "invalid source freshness configuration");
            open_job_incident(
                &pool,
                "job:source_freshness",
                operations::Severity::Error,
                "source_freshness_configuration",
                "Source freshness worker configuration is invalid",
                serde_json::json!({}),
            )
            .await;
        }
    }

    match data_maintenance::run(&pool).await {
        Ok(summary) => {
            tracing::info!(
                duplicate_candidates_upserted = summary.duplicate_candidates_upserted,
                quality_issues_observed = summary.quality_issues_observed,
                programs_expired = summary.programs_expired,
                "initial corporate benefit data maintenance complete"
            );
            resolve_job_incident(&pool, "job:corporate_benefit_maintenance").await;
        }
        Err(error) => {
            tracing::error!(error = %error, "initial corporate benefit data maintenance failed");
            open_job_incident(
                &pool,
                "job:corporate_benefit_maintenance",
                operations::Severity::Error,
                "corporate_benefit_maintenance",
                "Corporate benefit data maintenance failed",
                serde_json::json!({}),
            )
            .await;
        }
    }

    // Then the durable daily catch-up, as before the move: it runs data
    // maintenance too, so starting it alongside the steps above would run
    // the same maintenance twice at once.
    if let Err(error) = mazimi_worker::daily::run_due(&pool).await {
        tracing::error!(error=%error,"startup daily collection failed");
    }
}
