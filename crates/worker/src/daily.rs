//! Durable daily collection: restart catch-up, per-source leases and retries.
use crate::{
    data_maintenance, pipeline, source_monitoring,
    sources::{
        benefit_pages::BenefitPageFetcher,
        brand_catalog::{run_brand_catalog_ingestion, FtcBrandCatalogSource},
        financial::FssFinancialSource,
        gov_benefits::GovBenefitsSource,
        local_welfare::LocalWelfareSource,
        national_welfare::NationalWelfareSource,
        official_seeds::OfficialSeedDiscoverer,
        scholarship::ScholarshipSource,
        youth_center::YouthCenterSource,
        DataSource,
    },
};
use anyhow::{Context, Result};
use futures_util::FutureExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{future::Future, panic::AssertUnwindSafe, time::Duration};
use uuid::Uuid;

/// Attempts per Korean calendar day, the first one included. Hourly retries of
/// a deterministic failure would otherwise re-fetch a whole source up to 24
/// times and exhaust the data.go.kr daily quota.
const MAX_ATTEMPTS_PER_DAY: i32 = 3;

/// Wall-clock limit of one daily job (the claim lease is 35 minutes).
const JOB_TIME_LIMIT: Duration = Duration::from_secs(1800);

tokio::task_local! {
    /// Ingestion runs the daily job on this task started ([`note_job_run`]).
    static JOB_RUNS: std::sync::Arc<std::sync::Mutex<Vec<Uuid>>>;
}

/// Record `run_id` as a run of the daily job running on this task, so a
/// time-limit stop fails that run and no other. Outside a daily job it does
/// nothing.
pub(crate) fn note_job_run(run_id: Uuid) {
    let _ = JOB_RUNS.try_with(|runs| {
        runs.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(run_id)
    });
}

async fn job<F, Fut>(pool: &PgPool, name: &str, run: F) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    job_at(pool, name, chrono::Utc::now(), run).await
}

/// `job` with an explicit clock for the claim decision, so the Korean-day
/// retry cap can be tested at any wall-clock time (a relative "2 hours ago"
/// crosses Korean midnight when tests run between 00:00 and 02:00 KST).
async fn job_at<F, Fut>(
    pool: &PgPool,
    name: &str,
    now: chrono::DateTime<chrono::Utc>,
    run: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    job_with_limit(pool, name, now, JOB_TIME_LIMIT, run).await
}

async fn job_with_limit<F, Fut>(
    pool: &PgPool,
    name: &str,
    now: chrono::DateTime<chrono::Utc>,
    limit: Duration,
    run: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let token = Uuid::new_v4();
    // `attempts` counts claims within the current Korean day; the first claim
    // of a new day resets it to 1. Retries wait 50 minutes, not an hour: a
    // claim made a few seconds after an hourly tick would otherwise miss the
    // next tick by those seconds and retry two hours later.
    let claimed=sqlx::query_scalar::<_,String>("INSERT INTO worker_daily_jobs(job_name,last_attempt_at,status,attempts,lease_token,lease_until)
      VALUES($1,$4,'running',1,$2,$4+interval '35 minutes')
      ON CONFLICT(job_name) DO UPDATE SET last_attempt_at=$4,status='running',
          attempts=CASE WHEN worker_daily_jobs.last_attempt_at IS NULL
                          OR worker_daily_jobs.last_attempt_at < (date_trunc('day',$4 AT TIME ZONE 'Asia/Seoul') AT TIME ZONE 'Asia/Seoul')
                        THEN 1 ELSE worker_daily_jobs.attempts+1 END,
          lease_token=excluded.lease_token,lease_until=excluded.lease_until
      WHERE (worker_daily_jobs.last_success_at IS NULL OR worker_daily_jobs.last_success_at < (date_trunc('day',$4 AT TIME ZONE 'Asia/Seoul') AT TIME ZONE 'Asia/Seoul'))
        AND (worker_daily_jobs.lease_until IS NULL OR worker_daily_jobs.lease_until<$4)
        AND (worker_daily_jobs.last_attempt_at IS NULL OR worker_daily_jobs.last_attempt_at<$4-interval '50 minutes')
        AND (worker_daily_jobs.last_attempt_at IS NULL
             OR worker_daily_jobs.last_attempt_at < (date_trunc('day',$4 AT TIME ZONE 'Asia/Seoul') AT TIME ZONE 'Asia/Seoul')
             OR worker_daily_jobs.attempts < $3)
      RETURNING job_name").bind(name).bind(token).bind(MAX_ATTEMPTS_PER_DAY).bind(now).fetch_optional(pool).await?;
    if claimed.is_none() {
        return Ok(());
    }
    // A panic inside one source (for example a parser fed unexpected upstream
    // text) is contained here so the remaining jobs of this pass still run.
    let runs = std::sync::Arc::default();
    let result = JOB_RUNS
        .scope(
            std::sync::Arc::clone(&runs),
            tokio::time::timeout(limit, AssertUnwindSafe(run()).catch_unwind()),
        )
        .await;
    let (status, details) = match result {
        Ok(Ok(Ok(v))) => (
            if v.get("disabled").and_then(Value::as_bool) == Some(true) {
                "disabled"
            } else if v.get("degraded").and_then(Value::as_bool) == Some(true) {
                // Completed, so not retried today, but shown as a problem.
                "degraded"
            } else {
                "success"
            },
            v,
        ),
        Ok(Ok(Err(error))) => {
            tracing::error!(job=name,error=%crate::error_safety::persisted_error(&error),"daily collection failed; retry in one hour");
            (
                "failed",
                json!({"error":crate::error_safety::persisted_error(&error)}),
            )
        }
        Ok(Err(_panic)) => {
            tracing::error!(
                job = name,
                "daily collection panicked; the remaining jobs continue"
            );
            ("failed", json!({"error":"job panicked"}))
        }
        Err(_) => {
            let minutes = limit.as_secs() / 60;
            tracing::error!(
                job = name,
                limit_secs = limit.as_secs(),
                "daily job exceeded its time limit; its ingestion run is marked failed"
            );
            let runs = runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            record_timeout(pool, name, &runs, limit).await;
            (
                "failed",
                json!({"error":format!("job exceeded {minutes} minute limit")}),
            )
        }
    };
    if result_completed(status) {
        let fingerprint = format!("job_timeout:{name}");
        if let Err(error) = crate::operations::resolve_event(
            pool,
            &fingerprint,
            None,
            Some("the job completed within its time limit"),
        )
        .await
        {
            tracing::warn!(job = name, error = %crate::error_safety::persisted_error(&error), "failed to resolve job timeout incident");
        }
    }
    // A failed run keeps the last source reading (`source`, `output`) so the
    // next day's drop check still has yesterday's count.
    sqlx::query("UPDATE worker_daily_jobs SET status=$3,
      details=CASE WHEN $3='failed' THEN jsonb_strip_nulls(jsonb_build_object('source',details->'source','output',details->'output')) || $4 ELSE $4 END,
      lease_until=NULL,lease_token=NULL,
      last_success_at=CASE WHEN $3 IN ('success','degraded') THEN now() ELSE last_success_at END WHERE job_name=$1 AND lease_token=$2")
      .bind(name).bind(token).bind(status).bind(details).execute(pool).await?;
    tracing::info!(job = name, status, "daily collection recorded");
    Ok(())
}

/// A job that ran to the end, whatever it reported.
fn result_completed(status: &str) -> bool {
    matches!(status, "success" | "degraded" | "disabled")
}

/// The time limit drops the job's future, so the ingestion run it started
/// never records its own end. It is failed here (instead of by the stale
/// sweep an hour later), and `job_timeout:<job>` opens at error severity
/// until the job next completes. Only the runs this job started
/// ([`note_job_run`]) are failed: another process's run is not this job's.
/// Best effort: the job's own failure is recorded regardless.
async fn record_timeout(pool: &PgPool, name: &str, job_runs: &[Uuid], limit: Duration) {
    let message = format!(
        "Run stopped by the daily job time limit ({} minutes)",
        limit.as_secs() / 60
    );
    let runs = sqlx::query_scalar::<_, Uuid>(
        "UPDATE ingestion_runs SET status = 'failed', finished_at = now(), error_message = $2
         WHERE id = ANY($1) AND status = 'running' AND finished_at IS NULL
         RETURNING id",
    )
    .bind(job_runs)
    .bind(&message)
    .fetch_all(pool)
    .await;
    let runs = match runs {
        Ok(runs) => runs,
        Err(error) => {
            tracing::warn!(job = name, error = %crate::error_safety::persisted_error(&error), "failed to close the timed-out ingestion run");
            Vec::new()
        }
    };
    let fingerprint = format!("job_timeout:{name}");
    if let Err(error) = crate::operations::record_event(
        pool,
        &crate::operations::EventCommand {
            fingerprint: &fingerprint,
            severity: crate::operations::Severity::Error,
            component: "worker",
            event_type: "daily_job_timeout",
            subject_type: Some("worker_daily_job"),
            subject_id: Some(name),
            message: "A daily job was stopped by its time limit; it is retried within the day",
            details: json!({
                "job": name,
                "limit_secs": limit.as_secs(),
                "ingestion_runs_failed": runs,
            }),
        },
    )
    .await
    {
        tracing::warn!(job = name, error = %crate::error_safety::persisted_error(&error), "failed to record job timeout incident");
    }
}

async fn ingest<S: DataSource>(pool: &PgPool, source: S) -> Result<Value> {
    ingest_ref(pool, &source).await
}

/// Runs the source, then the "success but empty" guard. A degraded output
/// keeps the run's data and marks the daily job `degraded` (with an open
/// `source_output:<source>` error event) instead of `success`. A run that
/// kept no record on a source not allowed to be empty is degraded too, even
/// while earlier programmes are still served.
async fn ingest_ref<S: DataSource>(pool: &PgPool, source: &S) -> Result<Value> {
    let name = source.name();
    let previous = crate::source_health::previous_current(pool, name).await?;
    let run = pipeline::run_ingestion(pool, source).await?;
    let policy = crate::source_health::OutputPolicy::from_env(name)?;
    let output = crate::source_health::check(pool, name, previous, &policy).await?;
    let mut details = json!({
        "source": name,
        "output": output,
        "run": {"status": run.status, "kept": run.fetched, "upstream_seen": run.upstream_seen},
    });
    if !run.alerting_parts.is_empty() {
        details["run"]["unsettled"] = json!(run.alerting_parts);
    }
    if !run.blocked_parts.is_empty() {
        details["run"]["blocked"] = json!(run.blocked_parts);
    }
    let mut reasons = Vec::new();
    if run.unexpectedly_empty {
        reasons.push(format!(
            "run kept no record: {} upstream record(s), none passed the source filter",
            run.upstream_seen
        ));
    }
    // Only parts that need someone degrade the job; a blocked part with
    // nothing shown is listed in the digest instead.
    if !run.alerting_parts.is_empty() {
        reasons.push(format!(
            "parts of the catalogue not settled: {}",
            run.alerting_parts.join("; ")
        ));
    }
    if output.degraded() {
        reasons.push(format!("served programmes: {}", output.reasons.join(", ")));
    }
    if !reasons.is_empty() {
        details["degraded"] = json!(true);
        details["reason"] = json!(reasons.join("; "));
    }
    Ok(details)
}

/// A refresh token row whose JWT can no longer verify carries no reuse
/// signal, so it is swept here once a day instead of on every `/auth/refresh`
/// request. The extra day keeps a row visible to the API's replay and
/// grace-window checks past any clock skew between API and database.
pub async fn prune_expired_refresh_tokens(pool: &PgPool) -> Result<u64> {
    Ok(
        sqlx::query("DELETE FROM refresh_tokens WHERE expires_at < now() - interval '1 day'")
            .execute(pool)
            .await
            .context("expired refresh token sweep failed")?
            .rows_affected(),
    )
}
/// Nightly, low-priority AI fact extraction (A-09) under the same lease,
/// retry cap and panic isolation as the collection jobs. Provider failures
/// are counted in the run summary and keep the rule results; only a
/// database failure fails the job.
pub async fn run_ai_extraction(pool: &PgPool) -> Result<()> {
    job(pool, "nightly:ai_extraction", || async {
        match crate::ai_extraction::job::run_from_env(pool).await? {
            Some(summary) => Ok(serde_json::to_value(summary)?),
            None => Ok(json!({"disabled":true,"reason":"EXTRACTION_AI_ENABLED is not set"})),
        }
    })
    .await
}

/// Hourly drain of the AI extraction queue: programs whose source text
/// changed since the last sync (queued by a database trigger on ingestion).
/// Small by design (`EXTRACTION_AI_QUEUE_MAX_PROGRAMS`, 10-minute budget),
/// skipped while another extraction run is in progress, and contained like
/// the daily jobs (panic isolation, time cap).
pub async fn run_ai_extraction_queue(pool: &PgPool) -> Result<()> {
    let Some((provider, config)) = crate::ai_extraction::job::queue_config_from_env()? else {
        return Ok(());
    };
    let pruned = crate::ai_extraction::job::prune_queue(pool).await?;
    if pruned > 0 {
        tracing::info!(
            pruned,
            "inactive programmes dropped from the ai extraction queue"
        );
    }
    let (busy, queued): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM program_ai_extraction_runs
                        WHERE status = 'running' AND started_at > now() - interval '35 minutes'),
                EXISTS (SELECT 1 FROM program_ai_extraction_queue)",
    )
    .fetch_one(pool)
    .await?;
    if busy || !queued {
        return Ok(());
    }
    let run = AssertUnwindSafe(crate::ai_extraction::job::run_batch(
        pool, &provider, &config,
    ))
    .catch_unwind();
    match tokio::time::timeout(Duration::from_secs(15 * 60), run).await {
        Ok(Ok(Ok(summary))) => {
            tracing::info!(run_id = %summary.run_id, drained = summary.queue_drained,
                auto_applied = summary.auto.applied, "ai extraction queue drained");
            Ok(())
        }
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_panic)) => anyhow::bail!("ai extraction queue drain panicked"),
        Err(_) => anyhow::bail!("ai extraction queue drain exceeded 15 minutes"),
    }
}

/// A rolling-window confirmation whose 온통청년 sync failed or is stale has
/// confirmed nothing: once the stored confirmations age out those
/// programmes leave "open now". The job completes (no same-day retry) but
/// is `degraded`, so a broken sync is visible.
fn rolling_window_details(
    summary: &crate::condition_reviews::ConfirmationSummary,
) -> Result<Value> {
    let mut details = serde_json::to_value(summary)?;
    if let Some(reason) = &summary.degraded {
        details["degraded"] = json!(true);
        details["reason"] = json!(reason);
    }
    Ok(details)
}

fn disabled() -> Value {
    json!({"disabled":true,"reason":"source credentials or collection permission not configured"})
}

/// The newest scholarship dataset file a run actually used, from the
/// durable switch records (job details are overwritten on failure).
async fn last_scholarship_dataset(pool: &PgPool) -> Result<Option<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT details->>'path', details->>'dataset_date' FROM operational_events
         WHERE event_type = 'scholarship_dataset_switched'
           AND details->>'path' IS NOT NULL AND details->>'dataset_date' IS NOT NULL
         ORDER BY details->>'dataset_date' DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?)
}

/// A switch of the scholarship dataset (the provider publishes a new file a
/// few times a year) is an automatic action: recorded once per dataset.
async fn record_dataset_switch(
    pool: &PgPool,
    choice: &crate::sources::scholarship::DatasetChoice,
) -> Result<()> {
    if choice.pinned {
        return Ok(());
    }
    let fingerprint = format!("self_healing:scholarship_dataset:{}", choice.dataset_date);
    let seen: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM operational_events WHERE fingerprint = $1)",
    )
    .bind(&fingerprint)
    .fetch_one(pool)
    .await?;
    if seen {
        return Ok(());
    }
    crate::storage::record_self_healing(
        pool,
        &fingerprint,
        "scholarship_dataset_switched",
        "장학금 소스가 공공데이터포털의 최신 데이터셋으로 자동 전환되었습니다(비어 있지 않고 마감 전 공고가 있음을 확인).",
        serde_json::to_value(choice)?,
    )
    .await
}

/// The link resolver's daily self-metrics (one record per Korean day,
/// retried hourly twice). `degraded` when a source's application+notice
/// share fell by more than 2 points or more than 1% of its programmes show
/// a failing link (`link_resolver::metrics`). Disabled with the resolver.
pub async fn run_link_metrics(pool: &PgPool) -> Result<()> {
    job(pool, "daily:link_metrics", || async {
        if crate::link_resolver::Job::from_env()?.is_none() {
            return Ok(json!({"disabled":true,"reason":"LINK_RESOLVER_ENABLED is not set"}));
        }
        crate::link_resolver::metrics::record(pool).await
    })
    .await
}

/// Storage upkeep: allowlisted log retention (tightened automatically above
/// DB_SIZE_WARN_MB), database size and disk incidents. Scheduled on its own
/// (04:30 KST, retried hourly twice) and never at worker start, so a restart
/// does not add deletion and VACUUM load.
pub async fn run_storage(pool: &PgPool) -> Result<()> {
    job(pool, "daily:storage", || async {
        let config = crate::storage::StorageConfig::from_env()?;
        crate::storage::run_daily(pool, &config).await
    })
    .await
}

/// The 09:00 KST digest. Claimed once per Korean day through the daily-job
/// lease; before 09:00 KST it does nothing, so the hourly reconciliation
/// also catches up after a restart. Delivery failure is recorded and retried
/// up to three times that day; it never blocks other jobs.
pub async fn run_ops_digest(pool: &PgPool) -> Result<()> {
    run_ops_digest_at(pool, chrono::Utc::now()).await
}

async fn run_ops_digest_at(pool: &PgPool, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    if crate::digest::kst_hour(now) < 9 {
        return Ok(());
    }
    job_at(pool, "daily:ops_digest", now, || {
        crate::digest::send(pool, now)
    })
    .await
}

/// Missing credentials are explicitly disabled, never reported as a successful collection.
pub async fn run_due(pool: &PgPool) -> Result<()> {
    // A run left `running` by a restart or an aborted job must not look alive.
    match pipeline::fail_stale_ingestion_runs(pool).await {
        Ok(0) => {}
        Ok(count) => tracing::warn!(count, "stale ingestion runs marked failed"),
        Err(error) => tracing::warn!(
            error=%crate::error_safety::persisted_error(&error),
            "stale ingestion run sweep failed"
        ),
    }
    job(pool, "daily:expiry", || async {
        let s = data_maintenance::run(pool).await?;
        let refresh_tokens_pruned = prune_expired_refresh_tokens(pool).await?;
        Ok(json!({"expired":s.programs_expired,"refresh_tokens_pruned":refresh_tokens_pruned}))
    })
    .await?;
    macro_rules! optional_source {
        ($name:literal,$source:expr) => {
            job(pool, $name, || async {
                match $source {
                    Some(s) => ingest(pool, s).await,
                    None => Ok(disabled()),
                }
            })
            .await?;
        };
    }
    // from_env errors distinguish invalid configuration in the persisted job result.
    job(pool, "daily:youth", || async {
        match YouthCenterSource::from_env() {
            Ok(s) => ingest(pool, s).await,
            Err(_) => Ok(disabled()),
        }
    })
    .await?;
    // After the 온통청년 sync: confirm 상시 windows of reviewed programmes
    // from the payloads that sync stored, with no network request (off
    // unless ROLLING_WINDOW_CONFIRMATION_ENABLED).
    job(pool, "daily:rolling_window_confirmation", || async {
        match crate::condition_reviews::confirmation_config_from_env()? {
            Some(config) => {
                let summary = crate::condition_reviews::confirm_rolling_windows(
                    pool,
                    &config,
                    false,
                    chrono::Utc::now(),
                )
                .await?;
                rolling_window_details(&summary)
            }
            None => Ok(
                json!({"disabled":true,"reason":"ROLLING_WINDOW_CONFIRMATION_ENABLED is not set"}),
            ),
        }
    })
    .await?;
    job(pool, "daily:government", || async {
        match GovBenefitsSource::from_env() {
            Ok(s) => ingest(pool, s).await,
            Err(_) => Ok(disabled()),
        }
    })
    .await?;
    job(pool, "daily:scholarship", || async {
        let Ok(source) = ScholarshipSource::from_env() else {
            return Ok(disabled());
        };
        let source = source.with_last_used(last_scholarship_dataset(pool).await?);
        let mut details = ingest_ref(pool, &source).await?;
        if let Some(choice) = source.dataset_choice() {
            record_dataset_switch(pool, &choice).await?;
            details["dataset"] = serde_json::to_value(choice)?;
        }
        Ok(details)
    })
    .await?;
    optional_source!("daily:financial", FssFinancialSource::from_env());
    optional_source!("daily:national_welfare", NationalWelfareSource::from_env());
    optional_source!("daily:local_welfare", LocalWelfareSource::from_env());
    job(pool, "daily:brand_catalog", || async {
        match FtcBrandCatalogSource::from_env() {
            Ok(s) => {
                run_brand_catalog_ingestion(pool, &s).await?;
                Ok(json!({"source":"ftc"}))
            }
            Err(_) => Ok(disabled()),
        }
    })
    .await?;
    // Every due corporate/university target is serviced in bounded batches.
    job(pool, "daily:corporate_and_campus_discovery", || async {
        // Official seeds only. Search-engine APIs are no source: their terms
        // forbid storing, caching or processing results (Naver 2.3, Kakao).
        OfficialSeedDiscoverer::run(pool).await?;
        Ok(json!({"source": "official_seeds"}))
    })
    .await?;
    job(pool, "daily:benefit_revalidation", || async {
        let Some(fetcher) = BenefitPageFetcher::from_env()? else {
            return Ok(disabled());
        };
        let mut checked = 0;
        let mut failed = 0;
        for _ in 0..50 {
            let s = fetcher.run(pool).await?;
            checked += s.claimed;
            failed += s.failed;
            if s.claimed == 0 {
                break;
            }
        }
        anyhow::ensure!(
            failed == 0,
            "{failed} benefit pages failed; held and scheduled for retry"
        );
        Ok(json!({"checked":checked}))
    })
    .await?;
    job(pool, "daily:source_freshness", || async {
        // The freshness close runs first: a failing maintenance step must
        // not leave unverified programmes shown.
        let unlisted = pipeline::close_unlisted_programmes(pool).await?;
        let (grace, limit) = source_monitoring::freshness_policy_from_env()?;
        source_monitoring::run_source_maintenance(pool, grace, limit).await?;
        data_maintenance::run(pool).await?;
        Ok(json!({"complete":true, "closed_unlisted": unlisted}))
    })
    .await?;
    job(pool, "daily:domain_approvals", || async {
        let window = crate::domain_approvals::renewal_window_from_env()?;
        let summary =
            crate::domain_approvals::run(pool, window, |domain, paths, contact| async move {
                crate::sources::benefit_pages::preflight_robots(
                    &domain,
                    &paths,
                    &contact,
                    Duration::from_secs(20),
                )
                .await
            })
            .await?;
        Ok(serde_json::to_value(summary)?)
    })
    .await?;
    run_ops_digest_at(pool, chrono::Utc::now()).await?;
    // This is an actual database read/write even on a day with no new benefits.
    sqlx::query("INSERT INTO worker_daily_jobs(job_name,last_success_at,status,details) VALUES('worker:activity',now(),'success',jsonb_build_object('active_programs',(SELECT count(*) FROM programs WHERE is_active))) ON CONFLICT(job_name) DO UPDATE SET last_success_at=now(),status='success',details=excluded.details")
      .execute(pool).await.context("worker activity write failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    async fn job_row(pool: &PgPool, name: &str) -> (String, Value, i32) {
        sqlx::query_as("SELECT status,details,attempts FROM worker_daily_jobs WHERE job_name=$1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn age_last_attempt(
        pool: &PgPool,
        name: &str,
        clock: chrono::DateTime<chrono::Utc>,
        interval: &str,
    ) {
        sqlx::query(&format!(
            "UPDATE worker_daily_jobs SET last_attempt_at=$2-interval '{interval}' WHERE job_name=$1"
        ))
        .bind(name)
        .bind(clock)
        .execute(pool)
        .await
        .unwrap();
    }

    /// 12:00 Korean time on the current Korean day: "2 hours earlier" is then
    /// always the same Korean day, whatever the wall clock says.
    fn korean_noon() -> chrono::DateTime<chrono::Utc> {
        use chrono::{FixedOffset, TimeZone};
        let kst = FixedOffset::east_opt(9 * 3600).unwrap();
        let today = chrono::Utc::now().with_timezone(&kst).date_naive();
        kst.from_local_datetime(&today.and_hms_opt(12, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// Serves 온통청년 items exactly as `YouthCenterSource::fetch_all` would,
    /// without the network: same item type, same payload and hash derivation.
    struct FixtureYouth(Vec<Value>);

    impl DataSource for FixtureYouth {
        fn name(&self) -> &'static str {
            "youth_center"
        }

        async fn fetch_all(&self) -> Result<crate::sources::SourceFetch> {
            let records = self
                .0
                .iter()
                .map(|raw| {
                    let item: crate::sources::youth_center::YouthPolicyItem =
                        serde_json::from_value(raw.clone())?;
                    let payload = serde_json::to_value(&item)?;
                    Ok(crate::sources::RawRecord {
                        source_id: item.plcy_no.clone().expect("fixture items carry plcyNo"),
                        content_hash: crate::sources::content_hash(&payload),
                        payload,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(crate::sources::SourceFetch::complete(records))
        }
    }

    async fn test_pool() -> PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .expect("isolated test database is required")
    }

    const YOUTH_TYPO_ID: &str = "20260422005400212868";

    /// Production state on 2026-10-12: `daily:youth` failed 270 times because
    /// the provider ships `aplyYmd: "20260601 ~ 20250710"` for one record and
    /// the pre-8265d23 worker failed the whole run for it. That record never
    /// produced a programme row, so it is quarantined: the run is `success`,
    /// the daily job is `success`, and the open `ingestion_partial` incident
    /// (208 occurrences in production on 2026-10-16) resolves with a note.
    /// The stale counter only defers the first claim to the next Korean day.
    #[tokio::test]
    async fn youth_run_with_provider_date_typo_succeeds_and_rejects_only_that_record() {
        let clock = korean_noon();
        let pool = test_pool().await;
        let name = format!("test:youth_typo_{}", Uuid::new_v4().simple());
        let page: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/youth_center_page_inverted_window.json"
        ))
        .unwrap();
        let items = page["result"]["youthPolicyList"]
            .as_array()
            .unwrap()
            .clone();
        let ids: Vec<String> = items
            .iter()
            .map(|item| item["plcyNo"].as_str().unwrap().to_string())
            .collect();

        // The production incident this record kept open.
        crate::operations::record_event(
            &pool,
            &crate::operations::EventCommand {
                fingerprint: "ingestion_partial:youth_center",
                severity: crate::operations::Severity::Warning,
                component: "ingestion",
                event_type: "ingestion_partial_failure",
                subject_type: None,
                subject_id: None,
                message: "An ingestion run completed with rejected or failed records",
                details: json!({"source":"youth_center","rejected":1}),
            },
        )
        .await
        .unwrap();
        // Mirror the production row: failed, 270 attempts, last tried 00:10
        // KST of the current Korean day.
        sqlx::query(
            "INSERT INTO worker_daily_jobs(job_name,last_attempt_at,status,attempts,details)
             VALUES($1,$2-interval '11 hours 50 minutes','failed',270,'{\"error\":\"youth_center: upstream returned HTTP 403\"}')",
        )
        .bind(&name)
        .bind(clock)
        .execute(&pool)
        .await
        .unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        let source = FixtureYouth(items.clone());
        job_at(&pool, &name, clock, || async {
            counter.fetch_add(1, Ordering::SeqCst);
            ingest(&pool, source).await
        })
        .await
        .unwrap();
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "same Korean day stays capped"
        );

        // Next Korean day: the counter resets and the run completes.
        age_last_attempt(&pool, &name, clock, "1 day 11 hours 50 minutes").await;
        let counter = ran.clone();
        let source = FixtureYouth(items);
        job_at(&pool, &name, clock, || async {
            counter.fetch_add(1, Ordering::SeqCst);
            ingest(&pool, source).await
        })
        .await
        .unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        let (status, details, attempts) = job_row(&pool, &name).await;
        assert_eq!((status.as_str(), attempts), ("success", 1), "{details}");

        let (run_status, fetched, rejected, failed, version, quarantined): (
            String,
            i32,
            i32,
            i32,
            i32,
            i32,
        ) = sqlx::query_as(
            "SELECT r.status, r.total_fetched, r.total_rejected, r.total_failed, r.normalizer_version,
                    r.total_quarantined
             FROM ingestion_runs r JOIN ingestion_items i ON i.run_id = r.id
             WHERE r.source_name = 'youth_center' AND i.source_id = $1
             ORDER BY r.started_at DESC LIMIT 1",
        )
        .bind(YOUTH_TYPO_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                run_status.as_str(),
                fetched,
                rejected,
                failed,
                version,
                quarantined
            ),
            (
                "success",
                3,
                0,
                0,
                pipeline::normalizer_version("youth_center"),
                1
            )
        );
        let (item_status, item_error): (String, Option<String>) = sqlx::query_as(
            "SELECT i.status, i.error_message FROM ingestion_items i
             JOIN ingestion_runs r ON r.id = i.run_id
             WHERE r.source_name = 'youth_center' AND i.source_id = $1
             ORDER BY r.started_at DESC LIMIT 1",
        )
        .bind(YOUTH_TYPO_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(item_status, "quarantined");
        assert!(item_error
            .unwrap_or_default()
            .contains("application date range"));
        let (event_status, note): (String, Option<String>) = sqlx::query_as(
            "SELECT status, resolution_note FROM operational_events
             WHERE fingerprint = 'ingestion_partial:youth_center'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event_status, "resolved");
        assert!(note
            .unwrap_or_default()
            .contains("1 deterministic upstream defect"));

        let stored: Vec<String> = sqlx::query_scalar(
            "SELECT source_id FROM programs WHERE source_type = 'youth_center'
             AND source_id = ANY($1) ORDER BY source_id",
        )
        .bind(&ids)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(stored.len(), 2, "both valid records are stored: {stored:?}");
        assert!(!stored.iter().any(|id| id == YOUTH_TYPO_ID));

        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// Read-only dry run of the full 온통청년 catalogue through the real
    /// pipeline into an isolated database. `YOUTH_DRY_RUN` is a JSON array of
    /// `youthPolicyList` items saved from the provider:
    /// `YOUTH_DRY_RUN=/path/all.json TEST_DATABASE_URL=... cargo test -p
    /// mazimi-worker --lib youth_full_catalogue_dry_run -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs YOUTH_DRY_RUN export and an isolated database"]
    async fn youth_full_catalogue_dry_run() {
        let path = std::env::var("YOUTH_DRY_RUN").expect("YOUTH_DRY_RUN");
        let items: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let pool = test_pool().await;
        let name = format!("test:youth_dry_run_{}", Uuid::new_v4().simple());
        job_at(&pool, &name, korean_noon(), || async {
            ingest(&pool, FixtureYouth(items)).await
        })
        .await
        .unwrap();
        let (status, details, _) = job_row(&pool, &name).await;
        let run: (String, i32, i32, i32, i32, i32) = sqlx::query_as(
            "SELECT status, total_fetched, total_changed, total_renormalized, total_rejected, total_failed
             FROM ingestion_runs WHERE source_name = 'youth_center' ORDER BY started_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let items: Vec<(String, i64)> = sqlx::query_as(
            "SELECT i.status, count(*) FROM ingestion_items i
             WHERE i.run_id = (SELECT id FROM ingestion_runs WHERE source_name = 'youth_center'
                               ORDER BY started_at DESC LIMIT 1)
             GROUP BY 1 ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let rejected: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT i.source_id, i.error_message FROM ingestion_items i
             WHERE i.status IN ('rejected', 'failed') AND i.run_id = (SELECT id FROM ingestion_runs
               WHERE source_name = 'youth_center' ORDER BY started_at DESC LIMIT 1)",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let programs: Vec<(String, bool, i64)> = sqlx::query_as(
            "SELECT program_status, is_active, count(*) FROM programs
             WHERE source_type = 'youth_center' GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        println!("job={status} {details}\nrun(status,fetched,changed,renormalized,rejected,failed)={run:?}\nitems={items:?}\nrejected={rejected:?}\nprograms={programs:?}");
        assert_eq!(status, "success");
        pool.close().await;
    }

    fn youth_fixture_item(plcy_no: &str) -> Value {
        let page: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/youth_center_page_inverted_window.json"
        ))
        .unwrap();
        page["result"]["youthPolicyList"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["plcyNo"] == plcy_no)
            .unwrap()
            .clone()
    }

    async fn event_state(pool: &PgPool, fingerprint: &str) -> Option<(String, Option<String>)> {
        sqlx::query_as(
            "SELECT status, resolution_note FROM operational_events WHERE fingerprint = $1",
        )
        .bind(fingerprint)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// Serves a fixed fetch under its own source name, without the network.
    struct FixtureFetch(&'static str, crate::sources::SourceFetch);

    impl DataSource for FixtureFetch {
        fn name(&self) -> &'static str {
            self.0
        }

        async fn fetch_all(&self) -> Result<crate::sources::SourceFetch> {
            Ok(self.1.clone())
        }
    }

    /// A source whose fetch outlasts any test time limit.
    struct SlowFetch(&'static str);

    impl DataSource for SlowFetch {
        fn name(&self) -> &'static str {
            self.0
        }

        async fn fetch_all(&self) -> Result<crate::sources::SourceFetch> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(filtered_out(0))
        }
    }

    /// A source name no other test or production row uses.
    fn unique_source() -> &'static str {
        Box::leak(
            format!("test_src_{}", &Uuid::new_v4().simple().to_string()[..12]).into_boxed_str(),
        )
    }

    fn filtered_out(upstream_seen: usize) -> crate::sources::SourceFetch {
        crate::sources::SourceFetch::filtered(Vec::new(), upstream_seen, true)
    }

    async fn latest_run(pool: &PgPool, source: &str) -> (String, Option<String>) {
        sqlx::query_as(
            "SELECT status, error_message FROM ingestion_runs WHERE source_name = $1
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(source)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn forget_source(pool: &PgPool, source: &str) {
        for statement in [
            "DELETE FROM programs WHERE source_type = $1",
            "DELETE FROM source_snapshots WHERE source_name = $1",
            "DELETE FROM ingestion_runs WHERE source_name = $1",
            "DELETE FROM worker_daily_jobs WHERE job_name = 'test:' || $1",
            "DELETE FROM operational_events WHERE fingerprint LIKE '%:' || $1",
        ] {
            sqlx::query(statement)
                .bind(source)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// Before: a provider answering with an empty list (expired key, changed
    /// request contract) ended as `success` with 0 records. It now fails,
    /// opens `ingestion_failed:<source>` and the daily job retries.
    #[tokio::test]
    async fn a_provider_returning_no_records_fails_the_run() {
        let pool = test_pool().await;
        let source = unique_source();
        let job_name = format!("test:{source}");
        job_at(&pool, &job_name, korean_noon(), || async {
            ingest(&pool, FixtureFetch(source, filtered_out(0))).await
        })
        .await
        .unwrap();
        let (status, message) = latest_run(&pool, source).await;
        assert_eq!(status, "failed");
        assert!(
            message
                .unwrap_or_default()
                .contains("the provider returned no records"),
            "the stored error names the cause"
        );
        assert_eq!(
            event_state(&pool, &format!("ingestion_failed:{source}"))
                .await
                .unwrap()
                .0,
            "open"
        );
        let (job_status, _, _) = job_row(&pool, &job_name).await;
        assert_eq!(job_status, "failed", "the daily job retries within the day");
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// Before: 132 bank products, none passing the youth filter, recorded a
    /// `success` run every day. Now the run is `empty`, the daily job is
    /// `degraded`, `ingestion_empty:<source>` is open, and the next run with
    /// a record resolves it. Lifecycle reconciliation skips the empty fetch.
    #[tokio::test]
    async fn a_fully_filtered_fetch_is_empty_not_success_and_recovers() {
        let pool = test_pool().await;
        let source = unique_source();
        let job_name = format!("test:{source}");
        let fingerprint = format!("ingestion_empty:{source}");
        job_at(&pool, &job_name, korean_noon(), || async {
            ingest(&pool, FixtureFetch(source, filtered_out(132))).await
        })
        .await
        .unwrap();
        let (status, message) = latest_run(&pool, source).await;
        assert_eq!(status, "empty");
        assert!(message.unwrap_or_default().contains("returned 132 record"));
        assert_eq!(event_state(&pool, &fingerprint).await.unwrap().0, "open");
        let (job_status, details, _) = job_row(&pool, &job_name).await;
        assert_eq!(job_status, "degraded", "{details}");
        assert_eq!(details["run"]["upstream_seen"], 132, "{details}");
        assert!(details["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("run kept no record"));
        assert_eq!(
            event_state(&pool, &format!("ingestion_failed:{source}")).await,
            None,
            "an empty filter result is not a provider failure"
        );

        let record = |id: &str| {
            let payload = json!({"title": "청년 테스트 혜택", "id": id});
            crate::sources::RawRecord {
                source_id: id.to_string(),
                content_hash: crate::sources::content_hash(&payload),
                payload,
            }
        };
        let outcome = pipeline::run_ingestion(
            &pool,
            &FixtureFetch(
                source,
                crate::sources::SourceFetch::complete(vec![record("a1")]),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.status, outcome.fetched, outcome.unexpectedly_empty),
            ("success", 1, false)
        );
        let (status, note) = event_state(&pool, &fingerprint).await.unwrap();
        assert_eq!(status, "resolved");
        assert!(note.unwrap_or_default().contains("kept 1 record"));

        // An empty run between two successful ones is not the baseline the
        // next run's 80% drop check compares with, and closes nothing.
        let outcome = pipeline::run_ingestion(&pool, &FixtureFetch(source, filtered_out(5)))
            .await
            .unwrap();
        assert_eq!(outcome.status, "empty");
        let active: bool =
            sqlx::query_scalar("SELECT is_active FROM programs WHERE source_type = $1")
                .bind(source)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(active, "an empty fetch never counts a programme missing");
        let missing: i32 =
            sqlx::query_scalar("SELECT source_missing_runs FROM programs WHERE source_type = $1")
                .bind(source)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(missing, 0);
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// A source configured as allowed to be empty (a seasonal catalogue) is
    /// `empty` without an incident, even when the provider returned nothing.
    #[tokio::test]
    async fn a_source_allowed_to_be_empty_records_empty_without_an_incident() {
        let pool = test_pool().await;
        let source = unique_source();
        let flag = format!("SOURCE_ALLOW_EMPTY_{}", source.to_ascii_uppercase());
        // Strict first: the incident opens.
        pipeline::run_ingestion(&pool, &FixtureFetch(source, filtered_out(3)))
            .await
            .unwrap();
        let fingerprint = format!("ingestion_empty:{source}");
        assert_eq!(event_state(&pool, &fingerprint).await.unwrap().0, "open");

        std::env::set_var(&flag, "true");
        let filtered = pipeline::run_ingestion(&pool, &FixtureFetch(source, filtered_out(3))).await;
        let nothing = pipeline::run_ingestion(&pool, &FixtureFetch(source, filtered_out(0))).await;
        std::env::remove_var(&flag);
        for outcome in [filtered.unwrap(), nothing.unwrap()] {
            assert_eq!(
                (outcome.status, outcome.unexpectedly_empty),
                ("empty", false)
            );
        }
        let (status, note) = event_state(&pool, &fingerprint).await.unwrap();
        assert_eq!(status, "resolved");
        assert!(note.unwrap_or_default().contains("allowed to be empty"));
        assert_eq!(latest_run(&pool, source).await.0, "empty");
        forget_source(&pool, source).await;
        pool.close().await;
    }

    fn test_record(id: &str) -> crate::sources::RawRecord {
        let payload = json!({"title": "청년 테스트 금융상품", "id": id});
        crate::sources::RawRecord {
            source_id: id.to_string(),
            content_hash: crate::sources::content_hash(&payload),
            payload,
        }
    }

    fn part(prefix: &str, state: crate::sources::PartState) -> crate::sources::PartOutcome {
        crate::sources::PartOutcome {
            scope: crate::sources::PartScope::Prefix(prefix.into()),
            state,
            seen: 0,
            kept: 0,
            set_aside: 0,
        }
    }

    fn blocked() -> crate::sources::PartState {
        crate::sources::PartState::Unsettled(crate::sources::Unsettled {
            code: "non_json_body",
            detail: "the body is not JSON (deposit × 저축은행)".into(),
        })
    }

    /// A part the provider answers as empty (finlife `err_cd 000`,
    /// `total_count 0`: the shape of a sector without products, and of an
    /// outage) while programmes under its prefix are still shown is held and
    /// alerted on, never counted missing. A part that is empty with nothing
    /// stored is not reported.
    #[tokio::test]
    async fn an_empty_part_with_shown_programmes_is_held_and_alerted() {
        let pool = test_pool().await;
        let source = unique_source();
        let others = [
            "fss_rent_loan_1",
            "fss_rent_loan_2",
            "fss_rent_loan_3",
            "fss_rent_loan_4",
            "fss_rent_loan_5",
        ];
        let mut first: Vec<_> = others.iter().map(|id| test_record(id)).collect();
        first.push(test_record("fss_saving_0013175_NH"));
        pipeline::run_ingestion(
            &pool,
            &FixtureFetch(source, crate::sources::SourceFetch::complete(first)),
        )
        .await
        .unwrap();
        for run in 1..=4 {
            let mut records: Vec<_> = others.iter().map(|id| test_record(id)).collect();
            records.push(test_record("fss_rent_loan_6"));
            let mut fetch = crate::sources::SourceFetch::complete(records);
            fetch
                .parts
                .push(part("fss_saving_", crate::sources::PartState::Empty));
            fetch
                .parts
                .push(part("fss_060000_saving_", crate::sources::PartState::Empty));
            let outcome = pipeline::run_ingestion(&pool, &FixtureFetch(source, fetch))
                .await
                .unwrap();
            assert_eq!(
                outcome.alerting_parts,
                ["fss_saving_* answered_empty: the provider answered this part as empty"],
                "run {run}"
            );
            assert!(outcome.blocked_parts.is_empty());
        }
        let (active, missing): (bool, i32) = sqlx::query_as(
            "SELECT is_active, source_missing_runs FROM programs WHERE source_type = $1 AND source_id = 'fss_saving_0013175_NH'",
        )
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((active, missing), (true, 0));
        let (state, runs): (String, i32) = sqlx::query_as(
            "SELECT state, consecutive_unsettled_runs FROM ingestion_parts WHERE source_name = $1 AND id_prefix = 'fss_saving_'",
        )
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((state.as_str(), runs), ("unsettled", 4));
        let severity: String =
            sqlx::query_scalar("SELECT severity FROM operational_events WHERE fingerprint = $1")
                .bind(format!("ingestion_part_answered_empty:{source}"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(severity, "error");
        sqlx::query("DELETE FROM ingestion_parts WHERE source_name = $1")
            .bind(source)
            .execute(&pool)
            .await
            .unwrap();
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// A part that fails while nothing is shown under it is "blocked": named
    /// in the run and the digest's table, no open event, no degraded job.
    /// Once programmes are shown under a failing part it is an error, and
    /// the next run that settles every part resolves it.
    #[tokio::test]
    async fn a_blocked_part_is_quiet_and_a_part_with_shown_programmes_alerts() {
        let pool = test_pool().await;
        let source = unique_source();
        let fingerprint = format!("ingestion_part_non_json_body:{source}");
        let mut fetch =
            crate::sources::SourceFetch::complete(vec![test_record("fss_rent_loan_0010019_LN")]);
        fetch.parts.push(part("fss_030300_deposit_", blocked()));
        let details = ingest(&pool, FixtureFetch(source, fetch.clone()))
            .await
            .unwrap();
        assert_eq!(details["run"]["status"], "success");
        assert_eq!(
            details["run"]["blocked"],
            json!([
                "fss_030300_deposit_* non_json_body: the body is not JSON (deposit × 저축은행)"
            ])
        );
        assert!(details.get("degraded").is_none(), "{details}");
        let (status, message) = latest_run(&pool, source).await;
        assert_eq!(status, "success");
        assert!(message.unwrap_or_default().contains("fss_030300_deposit_*"));
        assert_ne!(
            event_state(&pool, &fingerprint).await.map(|e| e.0),
            Some("open".to_string())
        );

        // A programme of that part is shown: the same failure now alerts.
        fetch
            .records
            .push(test_record("fss_030300_deposit_0010345_X"));
        let mut listed = fetch.clone();
        listed.parts.clear();
        pipeline::run_ingestion(&pool, &FixtureFetch(source, listed))
            .await
            .unwrap();
        fetch.records.pop();
        let details = ingest(&pool, FixtureFetch(source, fetch.clone()))
            .await
            .unwrap();
        assert_eq!(details["degraded"], true);
        assert_eq!(event_state(&pool, &fingerprint).await.unwrap().0, "open");

        fetch.parts.clear();
        let outcome = pipeline::run_ingestion(&pool, &FixtureFetch(source, fetch))
            .await
            .unwrap();
        assert!(outcome.alerting_parts.is_empty());
        assert_eq!(
            event_state(&pool, &fingerprint).await.unwrap().0,
            "resolved"
        );
        sqlx::query("DELETE FROM ingestion_parts WHERE source_name = $1")
            .bind(source)
            .execute(&pool)
            .await
            .unwrap();
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// A programme the provider still lists but the source did not keep
    /// (filtered out, or its row set aside) opens one event naming it, and
    /// the next run that keeps or drops nothing shown resolves it.
    #[tokio::test]
    async fn a_listed_but_not_kept_programme_opens_an_event() {
        let pool = test_pool().await;
        let source = unique_source();
        let fingerprint = format!("ingestion_listed_not_kept:{source}");
        let ids = ["fss_rent_loan_0010019_LN", "fss_saving_0013175_NH"];
        let first: Vec<_> = ids.iter().map(|id| test_record(id)).collect();
        pipeline::run_ingestion(
            &pool,
            &FixtureFetch(source, crate::sources::SourceFetch::complete(first)),
        )
        .await
        .unwrap();
        let mut fetch = crate::sources::SourceFetch::filtered(Vec::new(), 2, false);
        fetch.filtered_out = vec![ids[0].into()];
        fetch.set_aside_ids = vec![ids[1].into()];
        pipeline::run_ingestion(&pool, &FixtureFetch(source, fetch))
            .await
            .unwrap();
        assert_eq!(event_state(&pool, &fingerprint).await.unwrap().0, "open");
        let details: Value =
            sqlx::query_scalar("SELECT details FROM operational_events WHERE fingerprint = $1")
                .bind(&fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(details["filtered_out"], json!([ids[0]]));
        assert_eq!(details["set_aside"], json!([ids[1]]));

        let kept: Vec<_> = ids.iter().map(|id| test_record(id)).collect();
        pipeline::run_ingestion(
            &pool,
            &FixtureFetch(source, crate::sources::SourceFetch::complete(kept)),
        )
        .await
        .unwrap();
        assert_eq!(
            event_state(&pool, &fingerprint).await.unwrap().0,
            "resolved"
        );
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// Security rule 4: every table in `public` has row-level security
    /// enabled and forced, so a new table without it fails here instead of
    /// waiting for a per-table assertion. Exceptions are reviewed.
    #[tokio::test]
    async fn every_public_table_has_forced_row_level_security() {
        const REVIEWED_EXCEPTIONS: &[&str] = &["_sqlx_migrations"];
        let pool = test_pool().await;
        let missing: Vec<String> = sqlx::query_scalar(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
               AND NOT (c.relrowsecurity AND c.relforcerowsecurity)
               AND c.relname <> ALL($1)
             ORDER BY 1",
        )
        .bind(REVIEWED_EXCEPTIONS)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(missing.is_empty(), "tables without forced RLS: {missing:?}");
        pool.close().await;
    }

    /// A fetch part with an empty prefix would cover every programme of the
    /// source: the run fails instead.
    #[tokio::test]
    async fn an_empty_part_prefix_fails_the_run() {
        let pool = test_pool().await;
        let source = unique_source();
        let mut fetch = crate::sources::SourceFetch::complete(vec![test_record("x1")]);
        fetch.parts.push(part("", blocked()));
        let error = pipeline::run_ingestion(&pool, &FixtureFetch(source, fetch))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty id prefix"), "{error}");
        forget_source(&pool, source).await;
        pool.close().await;
    }

    /// A finlife programme the provider has not listed for longer than the
    /// window is closed by the freshness job, whatever kept it (an unsettled
    /// part, an empty answer, failing runs), and the next run that lists it
    /// reopens it.
    #[tokio::test]
    async fn an_unlisted_programme_closes_after_the_window_and_reopens_when_listed() {
        let pool = test_pool().await;
        let id = format!(
            "fss_030300_saving_TEST{}",
            &Uuid::new_v4().simple().to_string()[..10]
        );
        let started: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let fetch = || {
            // The whole is unsettled, so reconciliation leaves every other
            // fss_financial row of the shared database alone.
            let mut fetch =
                crate::sources::SourceFetch::filtered(vec![test_record(&id)], 132, false);
            fetch.parts.push(part(
                "fss_030300_saving_",
                crate::sources::PartState::Settled,
            ));
            fetch
        };
        pipeline::run_ingestion(&pool, &FixtureFetch("fss_financial", fetch()))
            .await
            .unwrap();
        let state = || async {
            sqlx::query_as::<_, (bool, Option<String>)>(
                "SELECT is_active, retirement_reason FROM programs WHERE source_type = 'fss_financial' AND source_id = $1",
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        // A stale programme of a source the window does not cover stays.
        let other = unique_source();
        pipeline::run_ingestion(
            &pool,
            &FixtureFetch(
                other,
                crate::sources::SourceFetch::complete(vec![test_record("other_1")]),
            ),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE source_snapshots SET fetched_at = now() - interval '30 days' WHERE source_name = $1")
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();
        // Listed 6 days ago: inside the window, stays shown.
        for (days, active) in [(6, true), (8, false)] {
            sqlx::query("UPDATE source_snapshots SET fetched_at = now() - make_interval(days => $2) WHERE source_name = 'fss_financial' AND source_id = $1")
                .bind(&id)
                .bind(days)
                .execute(&pool)
                .await
                .unwrap();
            pipeline::close_unlisted_programmes(&pool).await.unwrap();
            let (is_active, reason) = state().await;
            assert_eq!(is_active, active, "{days} days");
            if !active {
                assert_eq!(reason.as_deref(), Some("source_removed"));
            }
        }
        let other_active: bool =
            sqlx::query_scalar("SELECT is_active FROM programs WHERE source_type = $1")
                .bind(other)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(other_active, "the window covers only the sources it names");
        forget_source(&pool, other).await;
        // Listed again with an unchanged payload: reopened.
        pipeline::run_ingestion(&pool, &FixtureFetch("fss_financial", fetch()))
            .await
            .unwrap();
        assert_eq!(state().await, (true, None));
        for statement in [
            "DELETE FROM programs WHERE source_type = 'fss_financial' AND source_id = $1",
            "DELETE FROM source_snapshots WHERE source_name = 'fss_financial' AND source_id = $1",
        ] {
            sqlx::query(statement)
                .bind(&id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "DELETE FROM ingestion_runs WHERE source_name = 'fss_financial' AND started_at >= $1",
        )
        .bind(started)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM ingestion_parts WHERE source_name = 'fss_financial' AND last_run_id IS NOT NULL AND updated_at >= $1")
            .bind(started)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// NH1934월복리적금 as finlife serves it reaches the catalogue with the
    /// age band the recommendation filter uses (`programs.min_age/max_age`,
    /// read by `structural_checks` through `rule_engine::age_gate`).
    #[tokio::test]
    async fn the_youth_bank_product_is_stored_with_its_age_band() {
        use crate::sources::financial::{
            product_record, FssBaseProduct, Product, YouthBasis, BANK, SAVING,
        };
        let pool = test_pool().await;
        let started: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/fss_bank_products_20261019.json"
        ))
        .unwrap();
        let mut nh = fixture["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["fin_prdt_nm"] == "NH1934월복리적금")
            .unwrap()
            .clone();
        let code = format!("TEST{}", &Uuid::new_v4().simple().to_string()[..10]);
        nh["fin_prdt_cd"] = json!(code);
        let product: FssBaseProduct = serde_json::from_value(nh).unwrap();
        let record = product_record(
            &SAVING,
            &BANK,
            &Product {
                base: product,
                options: Vec::new(),
            },
            YouthBasis::Band,
        );
        let source_id = record.source_id.clone();
        let outcome = pipeline::run_ingestion(
            &pool,
            &FixtureFetch(
                "fss_financial",
                // Incomplete: lifecycle reconciliation is skipped, so the
                // other fss_financial rows of the shared database are left
                // untouched by this one-record fetch.
                crate::sources::SourceFetch::filtered(vec![record], 132, false),
            ),
        )
        .await
        .unwrap();
        assert_eq!((outcome.status, outcome.fetched), ("success", 1));
        let (title, min_age, max_age, program_type): (String, Option<i32>, Option<i32>, String) =
            sqlx::query_as(
                "SELECT title, min_age, max_age, program_type FROM programs
                 WHERE source_type = 'fss_financial' AND source_id = $1",
            )
            .bind(&source_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, "농협은행주식회사 - NH1934월복리적금 (적금)");
        assert_eq!(
            (min_age, max_age, program_type.as_str()),
            (Some(19), Some(34), "financial_product")
        );
        let this_year = chrono::Datelike::year(&chrono::Utc::now());
        use mazimi_core::rule_engine::{age_gate, RuleVerdict};
        assert_eq!(
            age_gate(this_year - 40, min_age, max_age),
            RuleVerdict::Fail
        );
        assert_eq!(
            age_gate(this_year - 25, min_age, max_age),
            RuleVerdict::Pass
        );

        for statement in [
            "DELETE FROM programs WHERE source_type = 'fss_financial' AND source_id = $1",
            "DELETE FROM source_snapshots WHERE source_name = 'fss_financial' AND source_id = $1",
        ] {
            sqlx::query(statement)
                .bind(&source_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "DELETE FROM ingestion_runs WHERE source_name = 'fss_financial' AND started_at >= $1",
        )
        .bind(started)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    /// Production since the pre-quarantine replay: `renormalization_partial:
    /// youth_center` open, its run failed on `20260422005400212868`, and the
    /// replay never runs again (CLI only). A regular run that quarantines
    /// that record settles it; a failed record that is still stale with a
    /// programme row keeps it open.
    #[tokio::test]
    async fn a_regular_run_resolves_the_renormalization_incident_it_settled() {
        let pool = test_pool().await;
        let tag = &Uuid::new_v4().simple().to_string()[..16];
        let orphan = format!("9004{tag}");
        let published = format!("9005{tag}");
        let fingerprint = "renormalization_partial:youth_center";
        let mut orphan_item = youth_fixture_item(YOUTH_TYPO_ID);
        orphan_item["plcyNo"] = json!(orphan);

        // The replay run as the pre-quarantine code left it.
        let replay: Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_runs(source_name,status,started_at,finished_at,normalizer_version,run_kind,total_failed)
             VALUES('youth_center','failed',now()-interval '2 days',now()-interval '2 days',$1,'renormalization',2) RETURNING id",
        )
        .bind(pipeline::normalizer_version("youth_center"))
        .fetch_one(&pool)
        .await
        .unwrap();
        for id in [&orphan, &published] {
            sqlx::query(
                "INSERT INTO ingestion_items(id,run_id,source_id,status,error_message,created_at)
                 VALUES(gen_random_uuid(),$1,$2,'failed','normalized application date range is invalid',now())",
            )
            .bind(replay)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO source_snapshots(source_name,source_id,content_hash,raw_payload,normalizer_version)
             VALUES('youth_center',$1,'stale','{}'::jsonb,0)",
        )
        .bind(&published)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO programs(program_type,source_type,source_id,title) VALUES('policy','youth_center',$1,'게시된 공고')",
        )
        .bind(&published)
        .execute(&pool)
        .await
        .unwrap();
        crate::operations::record_event(
            &pool,
            &crate::operations::EventCommand {
                fingerprint,
                severity: crate::operations::Severity::Warning,
                component: "normalization",
                event_type: "stored_snapshot_renormalization_partial",
                subject_type: Some("ingestion_run"),
                subject_id: Some(&replay.to_string()),
                message: "Stored snapshot re-normalization completed with rejected records",
                details: json!({"source": "youth_center", "failed": 2}),
            },
        )
        .await
        .unwrap();

        // The orphan is quarantined, the published one is still stale.
        ingest(&pool, FixtureYouth(vec![orphan_item.clone()]))
            .await
            .unwrap();
        assert_eq!(event_state(&pool, fingerprint).await.unwrap().0, "open");

        // Once the published record is no longer stale, the next run settles it.
        sqlx::query(
            "UPDATE source_snapshots SET normalizer_version = $2
             WHERE source_name = 'youth_center' AND source_id = $1",
        )
        .bind(&published)
        .bind(pipeline::normalizer_version("youth_center"))
        .execute(&pool)
        .await
        .unwrap();
        ingest(&pool, FixtureYouth(vec![orphan_item]))
            .await
            .unwrap();
        let (status, note) = event_state(&pool, fingerprint).await.unwrap();
        assert_eq!(status, "resolved");
        assert!(note.unwrap_or_default().contains("settled all 2"));
        let orphan_snapshot: i32 = sqlx::query_scalar(
            "SELECT normalizer_version FROM source_snapshots WHERE source_name='youth_center' AND source_id=$1",
        )
        .bind(&orphan)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            orphan_snapshot, 0,
            "nothing was replayed, only the event resolved"
        );

        for id in [&orphan, &published] {
            for statement in [
                "DELETE FROM programs WHERE source_type='youth_center' AND source_id=$1",
                "DELETE FROM source_snapshots WHERE source_name='youth_center' AND source_id=$1",
            ] {
                sqlx::query(statement)
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(replay)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// The other direction of the quarantine: a published programme whose
    /// upstream record turns invalid is a real rejection and keeps alerting.
    #[tokio::test]
    async fn rejection_of_a_published_programme_still_raises_the_partial_incident() {
        let pool = test_pool().await;
        let id = format!("9003{}", &Uuid::new_v4().simple().to_string()[..16]);
        let mut item = youth_fixture_item("20261008005400113930");
        item["plcyNo"] = json!(id);
        ingest(&pool, FixtureYouth(vec![item.clone()]))
            .await
            .unwrap();
        crate::operations::resolve_event(&pool, "ingestion_partial:youth_center", None, None)
            .await
            .unwrap();

        item["aplyYmd"] = json!("20260601 ~ 20250710");
        ingest(&pool, FixtureYouth(vec![item])).await.unwrap();
        let (run_status, rejected, quarantined, item_status): (String, i32, i32, String) =
            sqlx::query_as(
                "SELECT r.status, r.total_rejected, r.total_quarantined, i.status
                 FROM ingestion_runs r JOIN ingestion_items i ON i.run_id = r.id
                 WHERE r.source_name = 'youth_center' AND i.source_id = $1
                 ORDER BY r.started_at DESC LIMIT 1",
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (
                run_status.as_str(),
                rejected,
                quarantined,
                item_status.as_str()
            ),
            ("success_with_rejections", 1, 0, "rejected")
        );
        assert_eq!(
            event_state(&pool, "ingestion_partial:youth_center")
                .await
                .unwrap()
                .0,
            "open"
        );
        sqlx::query("DELETE FROM programs WHERE source_type='youth_center' AND source_id=$1")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='youth_center' AND source_id=$1",
        )
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    /// Replay: a stale snapshot without a programme row that is rejected on
    /// every pass is quarantined; one with a programme row still fails the
    /// run and opens the incident. Once only quarantined snapshots remain the
    /// open incident resolves with a note.
    #[tokio::test]
    async fn replay_quarantines_broken_orphan_snapshots_but_fails_on_published_ones() {
        let pool = test_pool().await;
        let tag = &Uuid::new_v4().simple().to_string()[..16];
        let orphan = format!("9001{tag}");
        let published = format!("9002{tag}");
        // Fixtures left by an aborted earlier run would add failures.
        for table in [
            "source_snapshots WHERE source_name",
            "programs WHERE source_type",
        ] {
            sqlx::query(&format!(
                "DELETE FROM {table}='youth_center' AND source_id ~ '^900[123][0-9a-f]{{16}}$'"
            ))
            .execute(&pool)
            .await
            .unwrap();
        }
        for id in [&orphan, &published] {
            let mut raw = youth_fixture_item(YOUTH_TYPO_ID);
            raw["plcyNo"] = json!(id);
            let item: crate::sources::youth_center::YouthPolicyItem =
                serde_json::from_value(raw).unwrap();
            let payload = serde_json::to_value(&item).unwrap();
            sqlx::query(
                "INSERT INTO source_snapshots(source_name,source_id,content_hash,raw_payload,normalizer_version)
                 VALUES('youth_center',$1,$2,$3,0)",
            )
            .bind(id)
            .bind(crate::sources::content_hash(&payload))
            .bind(&payload)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO programs(program_type,source_type,source_id,title) VALUES('policy','youth_center',$1,'게시된 공고')",
        )
        .bind(&published)
        .execute(&pool)
        .await
        .unwrap();
        let item_status = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT i.status FROM ingestion_items i JOIN ingestion_runs r ON r.id = i.run_id
                     WHERE r.source_name = 'youth_center' AND r.run_kind = 'renormalization'
                       AND i.source_id = $1 ORDER BY r.started_at DESC LIMIT 1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        let summary = pipeline::renormalize_stored_snapshots(&pool, Some("youth_center"), 1000)
            .await
            .unwrap();
        let summary = serde_json::to_value(summary).unwrap();
        assert_eq!(item_status(orphan.clone()).await, "quarantined");
        assert_eq!(item_status(published.clone()).await, "failed");
        assert!(summary["quarantined"].as_u64().unwrap() >= 1, "{summary}");
        assert_eq!(summary["failed"], 1, "{summary}");
        assert_eq!(
            event_state(&pool, "renormalization_partial:youth_center")
                .await
                .unwrap()
                .0,
            "open"
        );

        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='youth_center' AND source_id=$1",
        )
        .bind(&published)
        .execute(&pool)
        .await
        .unwrap();
        let summary = pipeline::renormalize_stored_snapshots(&pool, Some("youth_center"), 1000)
            .await
            .unwrap();
        let summary = serde_json::to_value(summary).unwrap();
        assert_eq!(summary["failed"], 0, "{summary}");
        let run_status: String = sqlx::query_scalar(
            "SELECT status FROM ingestion_runs WHERE source_name='youth_center' AND run_kind='renormalization'
             ORDER BY started_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(run_status, "success");
        let (status, note) = event_state(&pool, "renormalization_partial:youth_center")
            .await
            .unwrap();
        assert_eq!(status, "resolved");
        assert!(note.unwrap_or_default().contains("quarantined"));

        sqlx::query("DELETE FROM programs WHERE source_type='youth_center' AND source_id=$1")
            .bind(&published)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='youth_center' AND source_id=$1",
        )
        .bind(&orphan)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    /// A failed run keeps yesterday's source reading, so the next successful
    /// run still compares against it.
    #[tokio::test]
    async fn a_failed_run_keeps_the_previous_source_reading() {
        let clock = korean_noon();
        let pool = test_pool().await;
        let tag = Uuid::new_v4().simple().to_string();
        let name = format!("daily:test_reading_{tag}");
        let source = format!("reading_{tag}");
        let reading = json!({"source": source, "output": {"current": 120}});
        job_at(&pool, &name, clock, || async { Ok(reading) })
            .await
            .unwrap();
        age_last_attempt(&pool, &name, clock, "2 days").await;
        sqlx::query(
            "UPDATE worker_daily_jobs SET last_success_at=$2-interval '2 days' WHERE job_name=$1",
        )
        .bind(&name)
        .bind(clock)
        .execute(&pool)
        .await
        .unwrap();
        job_at(&pool, &name, clock, || async {
            Err(anyhow::anyhow!("upstream rejected request"))
        })
        .await
        .unwrap();
        let (status, details, _) = job_row(&pool, &name).await;
        assert_eq!(status, "failed");
        assert_eq!(details["output"]["current"], 120, "{details}");
        assert!(details["error"].as_str().is_some());
        assert_eq!(
            crate::source_health::previous_current(&pool, &source)
                .await
                .unwrap(),
            Some(120)
        );
        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn a_panicking_job_is_recorded_failed_and_retries_are_capped_per_day() {
        let clock = korean_noon();
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .expect("isolated test database is required");
        let name = format!("test:retry_cap_{}", Uuid::new_v4().simple());
        let runs = Arc::new(AtomicUsize::new(0));

        // 1st attempt: a panic must neither escape nor hold the lease.
        let counter = runs.clone();
        job_at(&pool, &name, clock, || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            panic!("parser fed unexpected upstream text");
        })
        .await
        .unwrap();
        let (status, details, attempts) = job_row(&pool, &name).await;
        assert_eq!((status.as_str(), attempts), ("failed", 1));
        assert_eq!(details["error"], "job panicked");

        // 2nd and 3rd attempts fail normally once the hourly spacing allows.
        for expected in 2..=MAX_ATTEMPTS_PER_DAY {
            age_last_attempt(&pool, &name, clock, "2 hours").await;
            let counter = runs.clone();
            job_at(&pool, &name, clock, || async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("upstream rejected request"))
            })
            .await
            .unwrap();
            assert_eq!(job_row(&pool, &name).await.2, expected);
        }
        assert_eq!(runs.load(Ordering::SeqCst), MAX_ATTEMPTS_PER_DAY as usize);

        // Same Korean day, cap reached: the closure must not run again.
        age_last_attempt(&pool, &name, clock, "2 hours").await;
        let counter = runs.clone();
        job_at(&pool, &name, clock, || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"unexpected": true}))
        })
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), MAX_ATTEMPTS_PER_DAY as usize);
        assert_eq!(job_row(&pool, &name).await.0, "failed");

        // A new Korean day resets the counter and the job runs again.
        age_last_attempt(&pool, &name, clock, "2 days").await;
        let counter = runs.clone();
        job_at(&pool, &name, clock, || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"recovered": true}))
        })
        .await
        .unwrap();
        assert_eq!(
            runs.load(Ordering::SeqCst),
            MAX_ATTEMPTS_PER_DAY as usize + 1
        );
        let (status, details, attempts) = job_row(&pool, &name).await;
        assert_eq!((status.as_str(), attempts), ("success", 1));
        assert_eq!(details["recovered"], true);

        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// Production on 2026-10-21: 1,141 of 14,537 queue rows belonged to
    /// inactive programmes the drain never selects.
    #[tokio::test]
    async fn the_ai_queue_drops_programmes_that_became_inactive() {
        let pool = test_pool().await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active)
                 VALUES('policy','manual',$1,'큐 정리 검증','active',true) RETURNING id",
            )
            .bind(format!("queue-prune-{}", Uuid::new_v4().simple()))
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        sqlx::query("UPDATE programs SET is_active=false, program_status='closed' WHERE id=$1")
            .bind(ids[0])
            .execute(&pool)
            .await
            .unwrap();
        let queued = |pool: PgPool, ids: Vec<Uuid>| async move {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT program_id FROM program_ai_extraction_queue WHERE program_id = ANY($1) ORDER BY program_id",
            )
            .bind(ids)
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        assert_eq!(queued(pool.clone(), ids.clone()).await.len(), 2);
        // The count is shared with concurrent tests' prunes; the rows tell.
        crate::ai_extraction::job::prune_queue(&pool).await.unwrap();
        assert_eq!(queued(pool.clone(), ids.clone()).await, vec![ids[1]]);
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// A programme reactivated while the prune runs keeps the queue row its
    /// reactivation enqueued: the prune re-reads `is_active` under a lock
    /// instead of deleting by the snapshot it started with.
    #[tokio::test]
    async fn the_ai_queue_prune_keeps_a_programme_reactivated_meanwhile() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(
                &std::env::var("TEST_DATABASE_URL")
                    .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into()),
            )
            .await
            .unwrap();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active)
             VALUES('policy','manual',$1,'큐 정리 경합 검증','active',true) RETURNING id",
        )
        .bind(format!("queue-race-{}", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        // Closed with its old queue row still there.
        sqlx::query("UPDATE programs SET is_active=false, program_status='closed' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mut reopen = pool.begin().await.unwrap();
        sqlx::query("UPDATE programs SET is_active=true, program_status='active' WHERE id=$1")
            .bind(id)
            .execute(&mut *reopen)
            .await
            .unwrap();
        let prune = tokio::spawn({
            let pool = pool.clone();
            async move { crate::ai_extraction::job::prune_queue(&pool).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        reopen.commit().await.unwrap();
        prune.await.unwrap().unwrap();
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM program_ai_extraction_queue WHERE program_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(queued, 1, "the reactivated programme stays queued");
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn a_failed_or_stale_sync_degrades_the_rolling_window_job() {
        use crate::condition_reviews::ConfirmationSummary;
        let failing = ConfirmationSummary {
            candidates: 3,
            degraded: Some("latest 온통청년 sync failed".into()),
            ..ConfirmationSummary::default()
        };
        let details = rolling_window_details(&failing).unwrap();
        assert_eq!(details["degraded"], true, "{details}");
        assert_eq!(details["reason"], "latest 온통청년 sync failed");
        // Records no fresh sync returned are reported, not a degraded job.
        let healthy = ConfirmationSummary {
            candidates: 2,
            confirmed_rolling: 1,
            not_seen_recently: 1,
            ..ConfirmationSummary::default()
        };
        let details = rolling_window_details(&healthy).unwrap();
        assert!(details.get("degraded").is_none(), "{details}");
        assert_eq!(details["confirmed_rolling"], 1);
        assert_eq!(details["not_seen_recently"], 1);
    }

    /// Production on 2026-10-18/19/20: the first `gov_benefits` run was cut
    /// by the 30-minute limit, logged nothing at ERROR, raised no event and
    /// left its ingestion run `running` until the stale sweep an hour later.
    #[tokio::test]
    async fn a_timed_out_job_fails_its_run_and_opens_an_incident_until_it_completes() {
        let pool = test_pool().await;
        let name = format!("test:timeout_{}", Uuid::new_v4().simple());
        let source = unique_source();
        let foreign_source = unique_source();
        let fingerprint = format!("job_timeout:{name}");
        let foreign_run = Uuid::new_v4();
        job_with_limit(
            &pool,
            &name,
            chrono::Utc::now(),
            Duration::from_millis(1500),
            || async {
                // Another process's run (a manual bin, a second worker)
                // that started while this job ran: not this job's to fail.
                sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,$2,'running',now(),'ingestion')")
                    .bind(foreign_run)
                    .bind(foreign_source)
                    .execute(&pool)
                    .await?;
                pipeline::run_ingestion(&pool, &SlowFetch(source)).await?;
                Ok(json!({}))
            },
        )
        .await
        .unwrap();
        let (status, details, _) = job_row(&pool, &name).await;
        assert_eq!(status, "failed", "{details}");
        let (run_id, run_status, finished, error): (Uuid, String, bool, Option<String>) =
            sqlx::query_as(
                "SELECT id, status, finished_at IS NOT NULL, error_message FROM ingestion_runs WHERE source_name=$1",
            )
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((run_status.as_str(), finished), ("failed", true));
        assert!(error.unwrap_or_default().contains("time limit"));
        let foreign_status: String =
            sqlx::query_scalar("SELECT status FROM ingestion_runs WHERE id=$1")
                .bind(foreign_run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(foreign_status, "running", "another job's run is left alone");
        let (event_status, severity): (String, String) =
            sqlx::query_as("SELECT status, severity FROM operational_events WHERE fingerprint=$1")
                .bind(&fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (event_status.as_str(), severity.as_str()),
            ("open", "error")
        );

        // The next run that completes resolves the incident.
        age_last_attempt(&pool, &name, chrono::Utc::now(), "2 hours").await;
        job_with_limit(
            &pool,
            &name,
            chrono::Utc::now(),
            Duration::from_secs(30),
            || async { Ok(json!({"ok": true})) },
        )
        .await
        .unwrap();
        assert_eq!(job_row(&pool, &name).await.0, "success");
        let event_status: String =
            sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint=$1")
                .bind(&fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(event_status, "resolved");

        sqlx::query("DELETE FROM ingestion_runs WHERE id=ANY($1)")
            .bind([run_id, foreign_run])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM operational_events WHERE fingerprint=$1")
            .bind(&fingerprint)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// A job claimed a few seconds after the :10 tick is retried on the next
    /// tick, not two hours later (`daily:youth` failed at 15:10:04 and
    /// retried at 17:10 on 2026-10-18 and 2026-10-20).
    #[tokio::test]
    async fn a_failed_job_is_retried_on_the_next_hourly_tick() {
        let clock = korean_noon();
        let pool = test_pool().await;
        let name = format!("test:retry_tick_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO worker_daily_jobs(job_name,last_attempt_at,status,attempts,details)
             VALUES($1,$2-interval '59 minutes 56 seconds','failed',1,'{}')",
        )
        .bind(&name)
        .bind(clock)
        .execute(&pool)
        .await
        .unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = runs.clone();
        job_at(&pool, &name, clock, || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("upstream rejected request"))
        })
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the next tick retries");

        // Well inside the hour (a restart catch-up, say) it still waits.
        age_last_attempt(&pool, &name, clock, "40 minutes").await;
        let counter = runs.clone();
        job_at(&pool, &name, clock, || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        })
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        sqlx::query("DELETE FROM worker_daily_jobs WHERE job_name=$1")
            .bind(&name)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn expired_refresh_tokens_are_swept_daily_with_a_day_of_slack() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .expect("isolated test database is required");
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users(auth_provider,auth_provider_id,role,created_at,updated_at) \
             VALUES('test',$1,'user',now(),now()) RETURNING id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        let rows = [
            (Uuid::new_v4(), "now() - interval '2 days'"),  // swept
            (Uuid::new_v4(), "now() - interval '2 hours'"), // expired, inside slack
            (Uuid::new_v4(), "now() + interval '29 days'"), // live
        ];
        for (jti, expires_at) in &rows {
            sqlx::query(&format!(
                "INSERT INTO refresh_tokens(jti,user_id,expires_at) VALUES($1,$2,{expires_at})"
            ))
            .bind(jti)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(prune_expired_refresh_tokens(&pool).await.unwrap(), 1);
        let remaining: Vec<Uuid> = sqlx::query_scalar(
            "SELECT jti FROM refresh_tokens WHERE user_id=$1 ORDER BY expires_at",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, vec![rows[1].0, rows[2].0]);

        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
