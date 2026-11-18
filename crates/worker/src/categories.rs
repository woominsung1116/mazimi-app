//! Bounded backfill of the stored programme categories (migration
//! 20261023000002). A trigger keeps every new or changed row current; this
//! job fills rows written before the columns existed and recomputes every row
//! after a rule change bumps `program_category_version()`.
//!
//! Each run is bounded: [`BATCH`] rows per statement, at most `max_batches`
//! statements and no new batch after `max_duration`. Pending rows are read
//! `FOR UPDATE SKIP LOCKED`, so a run never waits on the ingestion's writes
//! and the next run goes on where this one stopped. The worker schedules it
//! every five minutes, never on the startup path (incident 2026-10-22).
//! Until a row is filled its categories are NULL and the API lists it only
//! under 전체.
use anyhow::Result;
use sqlx::PgPool;
use std::time::{Duration, Instant};

/// Rows per statement.
pub const BATCH: i64 = 500;

/// What one run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    pub updated: u64,
    /// Rows still pending after the run.
    pub remaining: i64,
}

const PENDING: &str = "category_version IS DISTINCT FROM program_category_version()";

/// One bounded run.
pub async fn refresh(pool: &PgPool, max_batches: usize, max_duration: Duration) -> Result<Refresh> {
    let started = Instant::now();
    let mut out = Refresh::default();
    for _ in 0..max_batches {
        if started.elapsed() >= max_duration {
            break;
        }
        let updated = sqlx::query(&format!(
            "UPDATE programs p SET
                 provider_category = program_provider_category(
                     p.program_type, p.provider_kind, p.benefit_category, p.title),
                 provider_categories = program_provider_categories(
                     p.program_type, p.provider_kind, p.benefit_category, p.title),
                 field_category = program_field_category(
                     p.program_type, p.source_type, p.provider_kind, p.benefit_category,
                     p.title, p.summary, p.raw_payload),
                 category_version = program_category_version()
             WHERE p.id IN (
                 SELECT id FROM programs WHERE {PENDING}
                 ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)"
        ))
        .bind(BATCH)
        .execute(pool)
        .await?
        .rows_affected();
        out.updated += updated;
        if updated == 0 {
            break;
        }
    }
    out.remaining = sqlx::query_scalar(&format!("SELECT count(*) FROM programs WHERE {PENDING}"))
        .fetch_one(pool)
        .await?;
    Ok(out)
}

/// The filter indexes (migrations 20261023000004 / 20261023000005 /
/// 20261026000302), by name
/// and the statement that builds each without blocking writes.
pub const INDEXES: [(&str, &str); 3] = [
    (
        "programs_field_category",
        "CREATE INDEX CONCURRENTLY programs_field_category ON programs (field_category)",
    ),
    // 20261026000302: the 기업혜택 browsing chip (mazimi_core::corporate_categories).
    (
        "programs_browse_category",
        "CREATE INDEX CONCURRENTLY programs_browse_category \
         ON programs (browse_category) WHERE browse_category IS NOT NULL",
    ),
    (
        "programs_provider_categories",
        "CREATE INDEX CONCURRENTLY programs_provider_categories \
         ON programs USING gin (provider_categories)",
    ),
];

/// Rebuild a filter index that is INVALID (a CREATE INDEX CONCURRENTLY that
/// failed part-way leaves one, and the migration's IF NOT EXISTS would then
/// skip it) or missing: DROP INDEX CONCURRENTLY, then CREATE INDEX
/// CONCURRENTLY, each its own statement outside a transaction, so writes to
/// programs never block. Returns the rebuilt names.
pub async fn repair_indexes(pool: &PgPool) -> Result<Vec<&'static str>> {
    let mut rebuilt = Vec::new();
    for (name, create) in INDEXES {
        let state: Option<bool> = sqlx::query_scalar(
            "SELECT i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
             WHERE c.relname = $1 AND c.relnamespace = 'public'::regnamespace",
        )
        .bind(name)
        .fetch_optional(pool)
        .await?;
        if state == Some(true) {
            continue;
        }
        tracing::warn!(
            index = name,
            invalid = state.is_some(),
            "rebuilding programme category index"
        );
        sqlx::raw_sql(&format!("DROP INDEX CONCURRENTLY IF EXISTS {name}"))
            .execute(pool)
            .await?;
        sqlx::raw_sql(create).execute(pool).await?;
        rebuilt.push(name);
    }
    Ok(rebuilt)
}

/// The scheduled run: repair the filter indexes if needed, then 20 batches
/// (10,000 rows) or 60 s. A failure is logged and the next tick retries;
/// stored categories stay.
pub async fn refresh_scheduled(pool: &PgPool) -> Option<Refresh> {
    if let Err(error) = repair_indexes(pool).await {
        tracing::error!(
            error = %crate::error_safety::persisted_error(&error),
            "programme category index repair failed; filters may scan"
        );
    }
    match refresh(pool, 20, Duration::from_secs(60)).await {
        Ok(summary) => Some(summary),
        Err(error) => {
            tracing::error!(
                error = %crate::error_safety::persisted_error(&error),
                "programme category refresh failed; stored categories kept"
            );
            None
        }
    }
}

/// The scheduled 기업혜택 browse-chip sweep
/// ([`mazimi_core::corporate_categories::refresh`]): rows whose inputs or
/// rules changed since their chip was stored, rows not filled yet (the
/// backfill after migration 20261026000301, done by the first tick) and rows
/// that left the tab; at most 4 batches (2,000 rows). The write paths store
/// the chip with their own writes; this catches every other path. A failure
/// is logged and the next tick retries; stored chips stay.
pub async fn refresh_browse_scheduled(
    pool: &PgPool,
) -> Option<mazimi_core::corporate_categories::Refresh> {
    let run = async {
        let mut connection = pool.acquire().await?;
        mazimi_core::corporate_categories::refresh(&mut connection, None, 4).await
    };
    match run.await {
        Ok(summary) => Some(summary),
        Err(error) => {
            tracing::error!(
                error = %crate::error_safety::persisted_error(&error),
                "기업혜택 browse-chip refresh failed; stored chips kept"
            );
            None
        }
    }
}

/// The browse-chip refresh at worker start, so the chips are not empty for
/// the first five minutes after migration 20261026000301 adds the column.
/// Guarded: when no 기업혜택 row lacks a chip (every start after the first)
/// it only runs one EXISTS query and returns `None`. Bounded like the sweep
/// (4 batches, rows held by others skipped), and spawned by `main`, so it never
/// blocks startup (incident 2026-10-22).
pub async fn refresh_browse_on_start(
    pool: &PgPool,
) -> Option<mazimi_core::corporate_categories::Refresh> {
    let run = async {
        let mut connection = pool.acquire().await?;
        if !mazimi_core::corporate_categories::any_unfilled(&mut connection).await? {
            return Ok(None);
        }
        mazimi_core::corporate_categories::refresh(&mut connection, None, 4)
            .await
            .map(Some)
    };
    match run.await {
        Ok(summary) => summary,
        Err(error) => {
            tracing::error!(
                error = %crate::error_safety::persisted_error(&error),
                "기업혜택 browse-chip start refresh failed; the sweep will fill them"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    async fn pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = PgPool::connect(&url).await.expect("test database");
        sqlx::migrate!("../../infra/migrations")
            .run(&pool)
            .await
            .expect("migrations");
        pool
    }

    #[tokio::test]
    async fn the_trigger_fills_new_rows_and_the_refresh_fills_the_backlog() {
        let pool = pool().await;
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,raw_payload) VALUES($1,'benefit','gov_benefits',$2,'청년 월세 지원','active','{}',false,$3)")
            .bind(id).bind(id.to_string()).bind(json!({"서비스분야": "주거·자립"}))
            .execute(&pool).await.unwrap();
        // The current rule version, not a literal: a rule bump must not break this.
        let version: i32 = sqlx::query_scalar("SELECT program_category_version()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let read = || async {
            sqlx::query_as::<_, (Option<String>, Option<String>, Option<i32>)>(
                "SELECT provider_category, field_category, category_version FROM programs WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(
            read().await,
            (Some("policy".into()), Some("housing".into()), Some(version))
        );
        // A row written before the columns existed (or under an old rule
        // version): writing only the category columns fires no trigger.
        sqlx::query("UPDATE programs SET provider_category = NULL, provider_categories = NULL, field_category = NULL, category_version = NULL WHERE id = $1")
            .bind(id).execute(&pool).await.unwrap();
        assert_eq!(read().await, (None, None, None));
        let mut remaining = i64::MAX;
        for _ in 0..1000 {
            let run = refresh(&pool, 4, Duration::from_secs(30)).await.unwrap();
            remaining = run.remaining;
            if read().await.2.is_some() {
                break;
            }
        }
        assert_eq!(
            read().await,
            (Some("policy".into()), Some("housing".into()), Some(version))
        );
        assert!(remaining >= 0);
        // A changed input recomputes through the trigger.
        sqlx::query("UPDATE programs SET title = '결혼축하금' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(read().await.1.as_deref(), Some("living"));
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// A failed concurrent build leaves an INVALID index; the job rebuilds it
    /// (and a missing one) and leaves a valid one alone.
    #[tokio::test]
    async fn invalid_or_missing_filter_indexes_are_rebuilt() {
        let pool = pool().await;
        let valid = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<bool>>(
                    "SELECT (SELECT i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE c.relname = $1)",
                )
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(repair_indexes(&pool).await.unwrap(), Vec::<&str>::new());
        // Simulate a build that failed part-way (test database superuser).
        sqlx::query("UPDATE pg_index SET indisvalid = false WHERE indexrelid = 'programs_field_category'::regclass")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(valid("programs_field_category").await, Some(false));
        sqlx::raw_sql("DROP INDEX CONCURRENTLY IF EXISTS programs_provider_categories")
            .execute(&pool)
            .await
            .unwrap();
        let rebuilt = repair_indexes(&pool).await.unwrap();
        assert_eq!(
            rebuilt,
            ["programs_field_category", "programs_provider_categories"]
        );
        assert_eq!(valid("programs_field_category").await, Some(true));
        assert_eq!(valid("programs_provider_categories").await, Some(true));
        assert_eq!(repair_indexes(&pool).await.unwrap(), Vec::<&str>::new());
    }

    #[tokio::test]
    async fn a_run_is_bounded_by_batches() {
        let pool = pool().await;
        let run = refresh(&pool, 0, Duration::from_secs(1)).await.unwrap();
        assert_eq!(run.updated, 0);
    }

    /// The browse-chip tests sweep every row of the shared test database, so
    /// they run one at a time.
    static BROWSE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A 기업혜택 row with no stored chip, as a write path that does not
    /// store it (or the migration) leaves one.
    async fn unfilled_corporate_row(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs(id,program_type,source_type,source_id,title,provider_name,program_status,provider_kind,benefit_category,is_active,regions)
             VALUES($1,'corporate_benefit','manual',$2,'Figma 학생 무료','Figma','always_open','corporate','software',false,ARRAY[]::text[])",
        )
        .bind(id)
        .bind(format!("browse-{id}"))
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn chip(pool: &PgPool, id: Uuid) -> Option<String> {
        sqlx::query_scalar("SELECT browse_category FROM programs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The sweep never waits on a row another transaction holds (feed
    /// ingestion, a write path): it locks rows in id order with SKIP LOCKED,
    /// leaves the held one pending, and the next sweep takes it.
    #[tokio::test]
    async fn the_browse_sweep_skips_a_row_another_transaction_holds() {
        let _one = BROWSE.lock().await;
        let pool = pool().await;
        let id = unfilled_corporate_row(&pool).await;
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM programs WHERE id = $1 FOR UPDATE")
            .bind(id)
            .execute(&mut *holder)
            .await
            .unwrap();
        let run = tokio::time::timeout(Duration::from_secs(20), refresh_browse_scheduled(&pool))
            .await
            .expect("the sweep must not wait on a row another transaction holds")
            .expect("sweep ran");
        assert!(run.remaining >= 1, "the held row stays pending: {run:?}");
        assert_eq!(chip(&pool, id).await, None);
        holder.rollback().await.unwrap();
        refresh_browse_scheduled(&pool).await.expect("sweep ran");
        assert_eq!(chip(&pool, id).await.as_deref(), Some("design"));
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// The start refresh fills a row with no chip, and does nothing once
    /// every row has one.
    #[tokio::test]
    async fn the_start_refresh_runs_only_while_a_chip_is_missing() {
        let _one = BROWSE.lock().await;
        let pool = pool().await;
        let id = unfilled_corporate_row(&pool).await;
        let run = refresh_browse_on_start(&pool)
            .await
            .expect("a chip was missing");
        assert!(run.updated >= 1, "{run:?}");
        assert_eq!(chip(&pool, id).await.as_deref(), Some("design"));
        // The shared database may hold other unfilled rows: fill them first.
        for _ in 0..100 {
            if refresh_browse_on_start(&pool).await.is_none() {
                break;
            }
        }
        assert!(
            refresh_browse_on_start(&pool).await.is_none(),
            "nothing missing: no run"
        );
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
