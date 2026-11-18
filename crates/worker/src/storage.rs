//! Storage upkeep: allowlisted log retention, index rebuilds after a large
//! cleanup, the database size guard with its daily size history and
//! days-to-limit projection, and the host disk reading.
//!
//! Hard rule: automatic cleanup only ever deletes from the tables in
//! [`RETENTION_ALLOWLIST`] (operational logs) and this module's own size
//! history (`storage_size_history`, [`SIZE_HISTORY_DAYS`]). Index rebuilds
//! touch only indexes of allowlisted tables. User data (users, profiles,
//! documents, bookmarks, programme states and their history, application
//! journeys, submissions and preparations, alert subscriptions) and the
//! catalogue (`programs`, `source_snapshots`) are never deleted here. The
//! allowlist is compiled in; no table name is read from the environment, and
//! every statement is a static string selected by exact match.

use crate::operations::{self, EventCommand, Severity};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::{Duration, Instant};

/// The only tables automatic retention may delete from.
pub const RETENTION_ALLOWLIST: &[&str] = &["ingestion_items", "corporate_door_reads"];
/// `corporate_door_reads` (one counts-only row per door read) is kept this
/// long, whatever the ingestion retention or the size guard says.
pub const DOOR_READS_RETENTION_DAYS: i32 = 180;
/// Retention never goes below this many days, whatever the configuration
/// or the size guard asks for.
pub const MIN_RETENTION_DAYS: i32 = 7;
const DEFAULT_RETENTION_DAYS: i32 = 14;
/// Daily work cap in batches of `INGESTION_ITEMS_RETENTION_BATCH` rows. A
/// backlog converges over a few days instead of one long run.
const MAX_BATCHES_PER_DAY: i64 = 5;
/// A cleanup this large (or of at least a tenth of the table) is followed by
/// index rebuilds: plain VACUUM leaves B-tree pages allocated, so
/// `pg_database_size` would not move (10-19: 254,934 rows deleted, 458 MB
/// before and after; REINDEX CONCURRENTLY brought it to 313 MB).
pub const REINDEX_MIN_DELETED_ROWS: u64 = 50_000;
/// No index rebuild starts after this much time since the storage job
/// started: the daily job is cut at 30 minutes, one rebuild may take up to
/// [`REINDEX_STATEMENT_TIMEOUT`], and a cut rebuild leaves an invalid index.
const REINDEX_BUDGET: Duration = Duration::from_secs(15 * 60);
/// `statement_timeout` for one rebuild on its dedicated connection. The
/// role default in production is 2 minutes, too short for a 100 MB index.
const REINDEX_STATEMENT_TIMEOUT: &str = "SET statement_timeout = '10min'";
/// An index is bloated, and rebuilt on any run (whatever the cleanup size),
/// when it is more than this many times its estimated packed size...
const BLOAT_FACTOR: f64 = 2.0;
/// ...and at least this large, so small indexes are never churned.
const BLOAT_MIN_BYTES: i64 = 8 * 1_048_576;
/// Safety margin on the packed-size estimate used for the headroom check.
const REBUILD_ESTIMATE_MARGIN: f64 = 1.2;
/// Size history retention (one small row per run).
pub const SIZE_HISTORY_DAYS: i32 = 400;
/// The growth rate is a least-squares fit over this many days of history.
const GROWTH_WINDOW_DAYS: i32 = 30;

struct RetentionPolicy {
    table: &'static str,
    /// `$1` retention days, `$2` limit: runs past the window that still have
    /// rows (`ingestion_runs` is small; `EXISTS` uses the run index).
    old_runs: &'static str,
    /// `$1` run id: one short transaction per run through
    /// `idx_ingestion_items_run`, never a table scan.
    delete_run: &'static str,
    vacuum: &'static str,
    /// Planner estimate and size: no full count of a large table.
    remaining: &'static str,
    /// The table's own indexes from the catalogue: name, bytes, valid, the
    /// heap's live-row estimate, the summed `pg_stats.avg_width` of the
    /// indexed columns (NULL unless every column has statistics, so an
    /// expression index has none), and the heap's size.
    indexes: &'static str,
}

const POLICIES: &[RetentionPolicy] = &[RetentionPolicy {
    table: "ingestion_items",
    // Per-record audit rows of runs older than the window. The run summary
    // (`ingestion_runs`) is kept; no reader needs items of old runs (they
    // are only read by the run_id of the run just made).
    old_runs: "SELECT r.id FROM ingestion_runs r
        WHERE r.started_at < now() - make_interval(days => $1)
          AND EXISTS (SELECT 1 FROM ingestion_items i WHERE i.run_id = r.id)
        ORDER BY r.started_at LIMIT $2",
    delete_run: "DELETE FROM ingestion_items WHERE run_id = $1",
    // Plain VACUUM: freed pages are reused, no exclusive lock (not FULL).
    vacuum: "VACUUM (ANALYZE) ingestion_items",
    remaining: "SELECT GREATEST(c.reltuples, 0)::bigint, pg_total_relation_size(c.oid)
        FROM pg_class c WHERE c.oid = 'public.ingestion_items'::regclass",
    indexes: "SELECT c.relname::text, pg_relation_size(c.oid), i.indisvalid,
          GREATEST(t.reltuples, 0)::float8,
          (SELECT CASE WHEN count(s.attname) = i.indnatts THEN sum(s.avg_width)::float8 END
             FROM unnest(i.indkey::int2[]) k(attnum)
             LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum AND k.attnum > 0
             LEFT JOIN pg_stats s ON s.schemaname = 'public' AND s.tablename = t.relname
                                 AND s.attname = a.attname),
          pg_relation_size(t.oid)
        FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
        JOIN pg_class t ON t.oid = i.indrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE i.indrelid = 'public.ingestion_items'::regclass AND n.nspname = 'public'
        ORDER BY i.indisvalid, pg_relation_size(c.oid), c.relname",
}, RetentionPolicy {
    table: "corporate_door_reads",
    // Read ids past the window, oldest first; one small row each.
    old_runs: "SELECT r.id FROM corporate_door_reads r
        WHERE r.read_at < now() - make_interval(days => $1)
        ORDER BY r.read_at LIMIT $2",
    delete_run: "DELETE FROM corporate_door_reads WHERE id = $1",
    vacuum: "VACUUM (ANALYZE) corporate_door_reads",
    remaining: "SELECT GREATEST(c.reltuples, 0)::bigint, pg_total_relation_size(c.oid)
        FROM pg_class c WHERE c.oid = 'public.corporate_door_reads'::regclass",
    indexes: "SELECT c.relname::text, pg_relation_size(c.oid), i.indisvalid,
          GREATEST(t.reltuples, 0)::float8, NULL::float8, pg_relation_size(t.oid)
        FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
        JOIN pg_class t ON t.oid = i.indrelid
        WHERE i.indrelid = 'public.corporate_door_reads'::regclass
        ORDER BY i.indisvalid, pg_relation_size(c.oid), c.relname",
}];

fn policy_for(table: &str) -> Result<&'static RetentionPolicy> {
    anyhow::ensure!(
        RETENTION_ALLOWLIST.contains(&table),
        "retention refused: {table:?} is not an allowlisted operational log table"
    );
    POLICIES
        .iter()
        .find(|policy| policy.table == table)
        .context("retention policy missing for an allowlisted table")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageConfig {
    /// `INGESTION_ITEMS_RETENTION_ENABLED` (default true). Off skips the
    /// deletion only; the size and disk readings still run.
    pub retention_enabled: bool,
    pub retention_days: i32,
    pub batch_size: i64,
    pub db_warn_mb: i64,
    pub db_error_mb: i64,
    /// `DB_SIZE_LIMIT_MB` (default 500, the plan's hard limit). Index
    /// rebuilds never let the database plus the index being rebuilt reach it.
    pub db_limit_mb: i64,
    pub disk_warn_gb: i64,
    pub disk_error_gb: i64,
}

impl StorageConfig {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            retention_enabled: !matches!(
                std::env::var("INGESTION_ITEMS_RETENTION_ENABLED")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "false" | "0" | "no"
            ),
            retention_days: env_i64(
                "INGESTION_ITEMS_RETENTION_DAYS",
                DEFAULT_RETENTION_DAYS.into(),
                MIN_RETENTION_DAYS.into(),
                365,
            )? as i32,
            batch_size: env_i64("INGESTION_ITEMS_RETENTION_BATCH", 50_000, 1_000, 100_000)?,
            db_warn_mb: env_i64("DB_SIZE_WARN_MB", 420, 1, 1_000_000)?,
            db_error_mb: env_i64("DB_SIZE_ERROR_MB", 470, 1, 1_000_000)?,
            db_limit_mb: env_i64("DB_SIZE_LIMIT_MB", 500, 1, 1_000_000)?,
            disk_warn_gb: env_i64("HOST_DISK_WARN_GB", 20, 1, 10_000)?,
            disk_error_gb: env_i64("HOST_DISK_ERROR_GB", 10, 1, 10_000)?,
        };
        anyhow::ensure!(
            config.db_warn_mb < config.db_error_mb,
            "DB_SIZE_WARN_MB must be below DB_SIZE_ERROR_MB"
        );
        anyhow::ensure!(
            config.db_error_mb <= config.db_limit_mb,
            "DB_SIZE_ERROR_MB must not exceed DB_SIZE_LIMIT_MB"
        );
        anyhow::ensure!(
            config.disk_error_gb < config.disk_warn_gb,
            "HOST_DISK_ERROR_GB must be below HOST_DISK_WARN_GB"
        );
        Ok(config)
    }
}

fn env_i64(name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<i64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max}"
    );
    Ok(value)
}

#[derive(Debug, Serialize)]
pub struct PruneSummary {
    pub table: &'static str,
    pub retention_days: i32,
    pub deleted: u64,
    pub runs_cleared: usize,
    pub max_rows: i64,
    /// Planner estimate after the run.
    pub remaining_rows_estimate: i64,
    pub table_bytes: i64,
    pub vacuumed: bool,
}

/// Deletes the rows of runs older than `retention_days` from one
/// allowlisted table, one run per statement, oldest first, until about
/// `max_rows` rows are gone (the run that crosses the cap is finished), then
/// runs a plain `VACUUM (ANALYZE)` when anything was deleted.
pub async fn prune(
    pool: &PgPool,
    table: &str,
    retention_days: i32,
    max_rows: i64,
) -> Result<PruneSummary> {
    let policy = policy_for(table)?;
    anyhow::ensure!(
        retention_days >= MIN_RETENTION_DAYS,
        "retention below the {MIN_RETENTION_DAYS}-day minimum refused"
    );
    anyhow::ensure!(max_rows > 0, "retention row cap must be positive");
    let runs: Vec<uuid::Uuid> = sqlx::query_scalar(policy.old_runs)
        .bind(retention_days)
        .bind(1_000_i64)
        .fetch_all(pool)
        .await
        .with_context(|| format!("{} retention run lookup failed", policy.table))?;
    let mut deleted = 0;
    let mut runs_cleared = 0;
    for run in runs {
        if deleted as i64 >= max_rows {
            break;
        }
        deleted += sqlx::query(policy.delete_run)
            .bind(run)
            .execute(pool)
            .await
            .with_context(|| format!("{} retention delete failed", policy.table))?
            .rows_affected();
        runs_cleared += 1;
    }
    let vacuumed = deleted > 0;
    if vacuumed {
        sqlx::query(policy.vacuum)
            .execute(pool)
            .await
            .with_context(|| format!("{} vacuum failed", policy.table))?;
    }
    let (remaining_rows_estimate, table_bytes): (i64, i64) =
        sqlx::query_as(policy.remaining).fetch_one(pool).await?;
    Ok(PruneSummary {
        table: policy.table,
        retention_days,
        deleted,
        runs_cleared,
        max_rows,
        remaining_rows_estimate,
        table_bytes,
        vacuumed,
    })
}

/// Whether a cleanup is large enough to rebuild the table's indexes:
/// [`REINDEX_MIN_DELETED_ROWS`] rows, or at least a tenth of the table as it
/// was before the cleanup.
pub fn should_reindex(deleted: u64, remaining_rows_estimate: i64) -> bool {
    let before = deleted.saturating_add(remaining_rows_estimate.max(0) as u64);
    deleted >= REINDEX_MIN_DELETED_ROWS || (deleted > 0 && deleted.saturating_mul(10) >= before)
}

/// A catalogue index name that may be spliced into a quoted identifier:
/// lowercase ASCII, digits and underscores, at most 63 bytes. Anything else
/// is skipped, never escaped.
fn safe_index_name(name: &str) -> bool {
    let mut chars = name.chars();
    (1..=63).contains(&name.len())
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The transient index a cut-off `REINDEX CONCURRENTLY` leaves behind
/// (`<name>_ccnew`, `<name>_ccnew1`, ...). PostgreSQL's documented recovery is
/// to drop it; it holds no data of its own.
fn leftover_reindex_index(name: &str) -> bool {
    ["_ccnew", "_ccold"].iter().any(|suffix| {
        name.rfind(suffix).is_some_and(|at| {
            name[at + suffix.len()..]
                .chars()
                .all(|c| c.is_ascii_digit())
        })
    })
}

/// Room for one rebuild: the database plus the new copy being built must
/// stay under the hard limit. `rebuilt_bytes` is [`rebuilt_size_estimate`],
/// not the bloated old size (at 458 MB a 110 MB bloated index rebuilds into
/// a few tens of MB).
pub fn has_headroom(database_bytes: i64, rebuilt_bytes: i64, limit_bytes: i64) -> bool {
    database_bytes.saturating_add(rebuilt_bytes) < limit_bytes
}

/// Bytes a freshly built B-tree over `rows` keys of `key_width` bytes
/// (summed `avg_width`) occupies: an 8-byte index tuple header plus the key,
/// MAXALIGNed to 8, plus a 4-byte line pointer, packed into 8 KB pages
/// (24-byte page header, 16-byte B-tree special space) at the default 90%
/// leaf fill, plus 2% for inner pages and the metapage.
pub fn packed_index_bytes(rows: f64, key_width: f64) -> i64 {
    const PAGE: f64 = 8192.0;
    let tuple = ((8.0 + key_width.max(0.0)) / 8.0).ceil() * 8.0 + 4.0;
    let per_page = ((PAGE - 24.0 - 16.0) * 0.9 / tuple).floor().max(1.0);
    let leaf_pages = (rows.max(0.0) / per_page).ceil();
    ((leaf_pages * 1.02 + 2.0) * PAGE) as i64
}

/// The size used for the headroom check: the packed estimate with a 20%
/// margin, never more than the current index. Without statistics (an
/// expression index, or a table never analysed) it falls back to the
/// current size capped at the heap's size.
pub fn rebuilt_size_estimate(
    index_bytes: i64,
    rows: f64,
    key_width: Option<f64>,
    heap_bytes: i64,
) -> i64 {
    match key_width {
        Some(width) => ((packed_index_bytes(rows, width) as f64 * REBUILD_ESTIMATE_MARGIN) as i64)
            .min(index_bytes),
        None => index_bytes.min(heap_bytes.max(0)),
    }
}

/// Bloated: more than [`BLOAT_FACTOR`] times its packed estimate and at least
/// [`BLOAT_MIN_BYTES`]. Unknown (no statistics) is not bloated.
pub fn is_bloated(index_bytes: i64, rows: f64, key_width: Option<f64>) -> bool {
    index_bytes >= BLOAT_MIN_BYTES
        && key_width.is_some_and(|width| {
            index_bytes as f64 > BLOAT_FACTOR * packed_index_bytes(rows, width) as f64
        })
}

/// Runs one maintenance statement on its own pooled connection with
/// [`REINDEX_STATEMENT_TIMEOUT`], then `RESET statement_timeout` so the
/// connection goes back with the role default; if the reset fails the
/// connection is closed instead of returned.
async fn run_maintenance(pool: &PgPool, statement: &str) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let result = async {
        sqlx::query(REINDEX_STATEMENT_TIMEOUT)
            .execute(&mut *conn)
            .await?;
        sqlx::query(statement).execute(&mut *conn).await.map(|_| ())
    }
    .await;
    if sqlx::query("RESET statement_timeout")
        .execute(&mut *conn)
        .await
        .is_err()
    {
        conn.close_on_drop();
    }
    result
}

#[derive(Debug, Serialize)]
pub struct IndexRebuild {
    pub index: String,
    pub bytes_before: i64,
    pub bytes_after: Option<i64>,
    /// The size the headroom check used for the new copy.
    pub estimated_rebuilt_bytes: i64,
    pub bloated: bool,
    /// reindexed | dropped_leftover | skipped_headroom | skipped_budget |
    /// skipped_invalid | skipped_unsafe_name | failed
    pub outcome: &'static str,
    pub database_bytes_at_start: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ReindexSummary {
    pub table: &'static str,
    pub limit_mb: f64,
    pub rebuilt: usize,
    pub indexes: Vec<IndexRebuild>,
}

/// Rebuilds indexes of one allowlisted table with `REINDEX INDEX
/// CONCURRENTLY`, one index per statement on a dedicated connection
/// ([`run_maintenance`]; autocommit, never inside a transaction: PostgreSQL
/// refuses it there). A valid index is rebuilt when `after_large_cleanup`,
/// or on any run when [`is_bloated`]: a skipped or failed rebuild of a
/// bloated index is therefore retried on the next daily run, with no flag to
/// keep. Before each rebuild the database size is measured again, and the
/// index is skipped unless [`has_headroom`] for its [`rebuilt_size_estimate`];
/// smaller indexes go first, so the space they free counts toward the larger
/// ones. No rebuild starts once [`REINDEX_BUDGET`] has passed since
/// `job_started`. Leftover `_ccnew`/`_ccold` indexes of a cut-off earlier
/// rebuild are dropped on every run (`DROP INDEX CONCURRENTLY`); any other
/// invalid index is reported and left alone. A failed rebuild is logged and
/// recorded, and the next index still runs.
pub async fn reindex(
    pool: &PgPool,
    table: &str,
    limit_bytes: i64,
    after_large_cleanup: bool,
    job_started: Instant,
) -> Result<ReindexSummary> {
    let policy = policy_for(table)?;
    #[allow(clippy::type_complexity)]
    let indexes: Vec<(String, i64, bool, f64, Option<f64>, i64)> = sqlx::query_as(policy.indexes)
        .fetch_all(pool)
        .await
        .with_context(|| format!("{} index lookup failed", policy.table))?;
    let mut results = Vec::with_capacity(indexes.len());
    for (index, bytes_before, valid, rows, key_width, heap_bytes) in indexes {
        let bloated = is_bloated(bytes_before, rows, key_width);
        if valid && !after_large_cleanup && !bloated {
            continue;
        }
        let estimated_rebuilt_bytes =
            rebuilt_size_estimate(bytes_before, rows, key_width, heap_bytes);
        let mut result = IndexRebuild {
            index: index.clone(),
            bytes_before,
            bytes_after: None,
            estimated_rebuilt_bytes,
            bloated,
            outcome: "reindexed",
            database_bytes_at_start: None,
        };
        let statement = if !safe_index_name(&index) {
            result.outcome = "skipped_unsafe_name";
            None
        } else if !valid && leftover_reindex_index(&index) {
            result.outcome = "dropped_leftover";
            Some(format!(
                "DROP INDEX CONCURRENTLY IF EXISTS public.\"{index}\""
            ))
        } else if !valid {
            result.outcome = "skipped_invalid";
            None
        } else if job_started.elapsed() >= REINDEX_BUDGET {
            result.outcome = "skipped_budget";
            None
        } else {
            let database: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())")
                .fetch_one(pool)
                .await?;
            result.database_bytes_at_start = Some(database);
            if has_headroom(database, estimated_rebuilt_bytes, limit_bytes) {
                Some(format!("REINDEX INDEX CONCURRENTLY public.\"{index}\""))
            } else {
                result.outcome = "skipped_headroom";
                None
            }
        };
        if let Some(statement) = statement {
            match run_maintenance(pool, &statement).await {
                Ok(_) => {
                    if result.outcome == "reindexed" {
                        result.bytes_after = sqlx::query_scalar(
                            "SELECT pg_relation_size(c.oid) FROM pg_class c
                             JOIN pg_namespace n ON n.oid = c.relnamespace
                             WHERE n.nspname = 'public' AND c.relname = $1",
                        )
                        .bind(&index)
                        .fetch_optional(pool)
                        .await?;
                    }
                }
                Err(error) => {
                    tracing::error!(
                        event = "storage_reindex_failed",
                        table = policy.table,
                        index = %index,
                        error = %crate::error_safety::persisted_error(&error),
                        "index rebuild failed; retried on the next daily run while the index stays bloated"
                    );
                    result.outcome = "failed";
                }
            }
        }
        if matches!(result.outcome, "skipped_headroom" | "skipped_budget") {
            tracing::warn!(
                event = "storage_reindex_skipped",
                table = policy.table,
                index = %result.index,
                outcome = result.outcome,
                bloated,
                "index rebuild skipped; retried on the next daily run while the index stays bloated"
            );
        }
        tracing::info!(
            event = "storage_reindex",
            table = policy.table,
            index = %result.index,
            outcome = result.outcome,
            bytes_before = result.bytes_before,
            bytes_after = ?result.bytes_after,
            "allowlisted index rebuild step"
        );
        results.push(result);
    }
    Ok(ReindexSummary {
        table: policy.table,
        limit_mb: mb(limit_bytes),
        rebuilt: results.iter().filter(|r| r.outcome == "reindexed").count(),
        indexes: results,
    })
}

#[derive(Debug, Serialize)]
pub struct DbSize {
    pub database_mb: f64,
    pub top_tables_mb: Vec<(String, f64)>,
    #[serde(skip)]
    pub database_bytes: i64,
    #[serde(skip)]
    pub top_tables_bytes: Vec<(String, i64)>,
}

pub async fn measure_db(pool: &PgPool) -> Result<DbSize> {
    let database: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())")
        .fetch_one(pool)
        .await?;
    let tables: Vec<(String, i64)> = sqlx::query_as(
        "SELECT relname::text, pg_total_relation_size(relid) FROM pg_stat_user_tables
         ORDER BY 2 DESC LIMIT 8",
    )
    .fetch_all(pool)
    .await?;
    Ok(DbSize {
        database_mb: mb(database),
        top_tables_mb: tables
            .iter()
            .map(|(name, bytes)| (name.clone(), mb(*bytes)))
            .collect(),
        database_bytes: database,
        top_tables_bytes: tables,
    })
}

/// Growth over the recent size history and the days left before the limit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Projection {
    /// Net growth per day (negative when cleanup outpaces inflow).
    pub growth_mb_per_day: f64,
    /// `None` when the database is not growing.
    pub days_to_limit: Option<f64>,
    pub samples: usize,
    pub span_days: f64,
}

/// Least-squares growth over `(unix seconds, bytes)` readings and the days
/// until `limit_bytes` at that rate from the newest reading. `None` with
/// fewer than two readings or less than a day between the first and last.
pub fn project(points: &[(f64, f64)], limit_bytes: f64) -> Option<Projection> {
    const DAY: f64 = 86_400.0;
    let (first, last) = (points.first()?, points.last()?);
    let span_days = (last.0 - first.0) / DAY;
    if points.len() < 2 || span_days < 1.0 {
        return None;
    }
    let n = points.len() as f64;
    let mean_t = points.iter().map(|p| (p.0 - first.0) / DAY).sum::<f64>() / n;
    let mean_b = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut cov, mut var) = (0.0, 0.0);
    for (t, b) in points {
        let dt = (t - first.0) / DAY - mean_t;
        cov += dt * (b - mean_b);
        var += dt * dt;
    }
    if var <= 0.0 {
        return None;
    }
    let slope = cov / var;
    let days_to_limit = (slope > 0.0).then(|| ((limit_bytes - last.1) / slope).max(0.0));
    Some(Projection {
        growth_mb_per_day: (slope / 1_048_576.0 * 100.0).round() / 100.0,
        days_to_limit: days_to_limit.map(|d| (d * 10.0).round() / 10.0),
        samples: points.len(),
        span_days: (span_days * 10.0).round() / 10.0,
    })
}

/// Appends one reading, drops readings older than [`SIZE_HISTORY_DAYS`]
/// (this table only), and projects from the last [`GROWTH_WINDOW_DAYS`].
pub async fn record_size(
    pool: &PgPool,
    size: &DbSize,
    limit_bytes: i64,
) -> Result<Option<Projection>> {
    let top: serde_json::Map<String, Value> = size
        .top_tables_bytes
        .iter()
        .map(|(name, bytes)| (name.clone(), json!(bytes)))
        .collect();
    sqlx::query("INSERT INTO storage_size_history(db_bytes, top_tables) VALUES($1, $2)")
        .bind(size.database_bytes)
        .bind(Value::Object(top))
        .execute(pool)
        .await
        .context("size history insert failed")?;
    sqlx::query(
        "DELETE FROM storage_size_history WHERE measured_at < now() - make_interval(days => $1)",
    )
    .bind(SIZE_HISTORY_DAYS)
    .execute(pool)
    .await
    .context("size history retention failed")?;
    let points: Vec<(f64, f64)> = sqlx::query_as(
        "SELECT extract(epoch FROM measured_at)::float8, db_bytes::float8 FROM storage_size_history
         WHERE measured_at > now() - make_interval(days => $1) ORDER BY measured_at",
    )
    .bind(GROWTH_WINDOW_DAYS)
    .fetch_all(pool)
    .await?;
    Ok(project(&points, limit_bytes as f64))
}

/// The self-healing record states what was measured, not what was hoped:
/// the database size before and after, and whether it went down.
pub fn healing_message(deleted: u64, rebuilt: usize, before_mb: f64, after_mb: f64) -> String {
    let action = if rebuilt > 0 {
        format!("수집 로그(ingestion_items) {deleted}행을 정리하고 인덱스 {rebuilt}개를 다시 만들었습니다")
    } else {
        format!("수집 로그(ingestion_items) {deleted}행을 정리했습니다")
    };
    if after_mb < before_mb {
        format!("{action}. 측정한 DB 용량: {before_mb}MB → {after_mb}MB.")
    } else {
        format!("{action}. 측정한 DB 용량은 줄지 않았습니다: {before_mb}MB → {after_mb}MB.")
    }
}

fn mb(bytes: i64) -> f64 {
    (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0
}

/// Severity of a size reading against warn/error thresholds (same unit).
pub fn size_level(value: f64, warn: i64, error: i64) -> Option<Severity> {
    if value >= error as f64 {
        Some(Severity::Error)
    } else if value >= warn as f64 {
        Some(Severity::Warning)
    } else {
        None
    }
}

/// Above the warn threshold retention drops straight to the minimum. Plain
/// VACUUM only makes space reusable, so the file size does not shrink; the
/// tighter window stays while the database is above the threshold.
pub fn effective_retention_days(configured: i32, database_mb: f64, warn_mb: i64) -> (i32, bool) {
    if database_mb >= warn_mb as f64 && configured > MIN_RETENTION_DAYS {
        (MIN_RETENTION_DAYS, true)
    } else {
        (configured, false)
    }
}

/// Free space of the filesystem holding the worker's root, which is the
/// container runtime's data directory on the host. `None` when `df` is not
/// available; the reading is advisory.
pub fn host_disk_free_gb() -> Option<f64> {
    let output = std::process::Command::new("df")
        .args(["-Pk", "/"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_df_available_gb(&String::from_utf8_lossy(&output.stdout))
}

fn parse_df_available_gb(text: &str) -> Option<f64> {
    let line = text.lines().nth(1)?;
    let available_kb: f64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some((available_kb / 1_048_576.0 * 10.0).round() / 10.0)
}

/// Records a completed automatic action: written as an event (audit trail and
/// digest input) and resolved at once, so it never sits in the open queue.
/// At most one record per fingerprint per Korean day: a retry or a second
/// pass the same day does not add another occurrence.
pub async fn record_self_healing(
    pool: &PgPool,
    fingerprint: &str,
    event_type: &str,
    message: &str,
    details: Value,
) -> Result<()> {
    let recorded_today: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM operational_events WHERE fingerprint = $1
           AND last_occurred_at >= (date_trunc('day', now() AT TIME ZONE 'Asia/Seoul') AT TIME ZONE 'Asia/Seoul'))",
    )
    .bind(fingerprint)
    .fetch_one(pool)
    .await?;
    if recorded_today {
        return Ok(());
    }
    operations::record_event(
        pool,
        &EventCommand {
            fingerprint,
            severity: Severity::Info,
            component: "self_healing",
            event_type,
            subject_type: None,
            subject_id: None,
            message,
            details,
        },
    )
    .await?;
    operations::resolve_event(pool, fingerprint, None, Some("automatic action completed")).await?;
    Ok(())
}

async fn threshold_event(
    pool: &PgPool,
    fingerprint: &str,
    event_type: &str,
    level: Option<Severity>,
    message: &str,
    details: Value,
) -> Result<()> {
    match level {
        Some(severity) => {
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint,
                    severity,
                    component: "storage",
                    event_type,
                    subject_type: None,
                    subject_id: None,
                    message,
                    details,
                },
            )
            .await?;
        }
        None => {
            operations::resolve_event(pool, fingerprint, None, Some("back below the threshold"))
                .await?;
        }
    }
    Ok(())
}

/// The daily storage job: measure, tighten retention when the database is
/// above the warn threshold, prune the allowlisted log table, rebuild its
/// indexes after a large cleanup, measure again, keep the size history, and
/// keep the size and disk incidents in step with the measured readings.
pub async fn run_daily(pool: &PgPool, config: &StorageConfig) -> Result<Value> {
    let job_started = Instant::now();
    let limit_bytes = config.db_limit_mb.saturating_mul(1_048_576);
    let before = measure_db(pool).await?;
    let (retention_days, tightened) = if config.retention_enabled {
        effective_retention_days(config.retention_days, before.database_mb, config.db_warn_mb)
    } else {
        (config.retention_days, false)
    };
    let pruned = if config.retention_enabled {
        Some(
            prune(
                pool,
                "ingestion_items",
                retention_days,
                config.batch_size * MAX_BATCHES_PER_DAY,
            )
            .await?,
        )
    } else {
        None
    };
    // Door reads: their own fixed window, only while retention is on.
    if config.retention_enabled {
        if let Err(error) = prune(
            pool,
            "corporate_door_reads",
            DOOR_READS_RETENTION_DAYS,
            config.batch_size * MAX_BATCHES_PER_DAY,
        )
        .await
        {
            tracing::error!(
                event = "storage_door_reads_retention_failed",
                error = %crate::error_safety::persisted_error(&error),
                "door read retention failed; the size guard still runs"
            );
        }
    }
    let deleted = pruned.as_ref().map_or(0, |pruned| pruned.deleted);
    let after_large_cleanup = pruned
        .as_ref()
        .is_some_and(|p| should_reindex(p.deleted, p.remaining_rows_estimate));
    // Every run: bloated indexes (and leftovers) are handled even without a
    // large cleanup. Best effort: a failed step must not suppress the alert.
    let mut reindex_step_failed = false;
    let reindexed = match reindex(
        pool,
        "ingestion_items",
        limit_bytes,
        after_large_cleanup,
        job_started,
    )
    .await
    {
        Ok(summary) if after_large_cleanup || !summary.indexes.is_empty() => Some(summary),
        Ok(_) => None,
        Err(error) => {
            tracing::error!(
                event = "storage_reindex_failed",
                error = %crate::error_safety::persisted_error(&error),
                "index rebuild step failed; the size guard still runs"
            );
            reindex_step_failed = true;
            None
        }
    };
    let rebuilt = reindexed.as_ref().map_or(0, |r| r.rebuilt);
    // Re-measured after the rebuild: this is the number the warning is
    // judged by, and the only one that can go down.
    let after = measure_db(pool).await?;
    let projection = match record_size(pool, &after, limit_bytes).await {
        Ok(projection) => projection,
        Err(error) => {
            tracing::error!(
                event = "storage_size_history_failed",
                error = %crate::error_safety::persisted_error(&error),
                "size history not recorded; the size guard still runs"
            );
            None
        }
    };
    if tightened || rebuilt > 0 {
        tracing::warn!(
            event = "storage_self_healing",
            database_mb_before = before.database_mb,
            database_mb_after = after.database_mb,
            retention_days,
            deleted,
            rebuilt,
            "storage cleanup finished; sizes are measured readings"
        );
        let (fingerprint, event_type) = if tightened {
            (
                "self_healing:ingestion_retention_tightened",
                "ingestion_retention_tightened",
            )
        } else {
            ("self_healing:ingestion_reindexed", "ingestion_reindexed")
        };
        let mut message = healing_message(deleted, rebuilt, before.database_mb, after.database_mb);
        if tightened {
            message =
                format!("DB 용량이 경고 기준을 넘어 보존 기간을 최소값으로 줄였습니다. {message}");
        }
        record_self_healing(
            pool,
            fingerprint,
            event_type,
            &message,
            json!({
                "configured_days": config.retention_days,
                "applied_days": retention_days,
                "database_mb_before": before.database_mb,
                "database_mb_after": after.database_mb,
                "reduced": after.database_mb < before.database_mb,
                "deleted": deleted,
                "indexes_rebuilt": rebuilt,
                "reindex": reindexed,
            }),
        )
        .await?;
    }

    let db_level = size_level(after.database_mb, config.db_warn_mb, config.db_error_mb);
    match db_level {
        Some(Severity::Error) | Some(Severity::Critical) => tracing::error!(
            event = "db_size_above_error",
            database_mb = after.database_mb,
            threshold_mb = config.db_error_mb,
            "database size above DB_SIZE_ERROR_MB"
        ),
        Some(_) => tracing::warn!(
            event = "db_size_above_warn",
            database_mb = after.database_mb,
            threshold_mb = config.db_warn_mb,
            "database size above DB_SIZE_WARN_MB"
        ),
        None => {}
    }
    threshold_event(
        pool,
        "storage:db_size",
        "db_size_above_threshold",
        db_level,
        "데이터베이스 용량이 기준을 넘었습니다. 요금제 한도와 큰 테이블을 확인하세요.",
        json!({
            "database_mb": after.database_mb,
            "database_mb_before_cleanup": before.database_mb,
            "warn_mb": config.db_warn_mb,
            "error_mb": config.db_error_mb,
            "limit_mb": config.db_limit_mb,
            "growth_mb_per_day": projection.map(|p| p.growth_mb_per_day),
            "projected_days_to_limit": projection.and_then(|p| p.days_to_limit),
            "top_tables_mb": after.top_tables_mb,
        }),
    )
    .await?;

    let disk_free_gb = host_disk_free_gb();
    if let Some(free) = disk_free_gb {
        // Lower free space is worse: invert the comparison.
        let level = if free < config.disk_error_gb as f64 {
            Some(Severity::Error)
        } else if free < config.disk_warn_gb as f64 {
            Some(Severity::Warning)
        } else {
            None
        };
        if level.is_some() {
            tracing::warn!(
                event = "host_disk_low",
                free_gb = free,
                "host disk free space low"
            );
        }
        threshold_event(
            pool,
            "storage:host_disk",
            "host_disk_low",
            level,
            "서버 디스크 여유 공간이 기준보다 적습니다. 호스트 정리 타이머 상태를 확인하세요.",
            json!({
                "free_gb": free,
                "warn_gb": config.disk_warn_gb,
                "error_gb": config.disk_error_gb,
            }),
        )
        .await?;
    }

    let degraded_reason = reindex_degraded_reason(reindex_step_failed, reindexed.as_ref());
    let mut details = json!({
        "retention": pruned,
        "retention_enabled": config.retention_enabled,
        "retention_tightened": tightened,
        "reindex": reindexed,
        "projection": projection,
        "database_mb_before": before.database_mb,
        "database_mb": after.database_mb,
        "db_level": db_level.map(Severity::as_str),
        "top_tables_mb": after.top_tables_mb,
        "host_disk_free_gb": disk_free_gb,
    });
    if let Some(reason) = degraded_reason {
        details["degraded"] = json!(true);
        details["reason"] = json!(reason);
    }
    Ok(details)
}

/// A bloated index that could not be rebuilt (the statement failed, or the
/// database lacked the headroom to build its copy) keeps growing until a
/// later run manages it, so the storage job completes as `degraded`
/// instead of `success`. A rebuild deferred by the time budget is retried
/// on the next run and is not a problem by itself.
fn reindex_degraded_reason(step_failed: bool, summary: Option<&ReindexSummary>) -> Option<String> {
    if step_failed {
        return Some("index rebuild step failed".into());
    }
    let stuck: Vec<String> = summary?
        .indexes
        .iter()
        .filter(|index| matches!(index.outcome, "failed" | "skipped_headroom"))
        .map(|index| format!("{} {}", index.index, index.outcome))
        .collect();
    (!stuck.is_empty()).then(|| format!("index rebuild not done: {}", stuck.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_or_headroom_skipped_rebuild_degrades_the_job() {
        let index = |outcome: &'static str| IndexRebuild {
            index: format!("idx_{outcome}"),
            bytes_before: 1,
            bytes_after: None,
            estimated_rebuilt_bytes: 1,
            bloated: true,
            outcome,
            database_bytes_at_start: None,
        };
        let summary = |outcomes: &[&'static str]| ReindexSummary {
            table: "ingestion_items",
            limit_mb: 500.0,
            rebuilt: 0,
            indexes: outcomes.iter().map(|o| index(o)).collect(),
        };
        assert_eq!(
            reindex_degraded_reason(true, None).as_deref(),
            Some("index rebuild step failed")
        );
        assert_eq!(
            reindex_degraded_reason(
                false,
                Some(&summary(&["reindexed", "failed", "skipped_headroom"]))
            )
            .as_deref(),
            Some(
                "index rebuild not done: idx_failed failed, idx_skipped_headroom skipped_headroom"
            )
        );
        for fine in [
            &["reindexed", "skipped_budget", "dropped_leftover"][..],
            &[],
        ] {
            assert_eq!(reindex_degraded_reason(false, Some(&summary(fine))), None);
        }
        assert_eq!(reindex_degraded_reason(false, None), None);
    }
    use uuid::Uuid;

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required")
    }

    /// Audit L6: every `DELETE FROM` in the worker's non-test code names a
    /// table of this reviewed list. A new automatic delete fails here until
    /// it is reviewed (user data is never deleted; operational rows only).
    /// `assistant_conversations` is the assistant's own retention window
    /// (`assistant_agent.rs`), listed as found, not as a new decision.
    #[test]
    fn every_worker_delete_names_a_reviewed_table() {
        const REVIEWED: &[&str] = &[
            "assistant_action_audit",
            "assistant_conversations",
            "assistant_notification_outbox",
            "assistant_proactive_deliveries",
            "assistant_program_fingerprints",
            "corporate_benefit_identities",
            "corporate_door_reads",
            "corporate_offer_facts",
            "eligibility_rules",
            "ingestion_items",
            "program_ai_extraction_queue",
            "program_ai_field_overrides",
            "program_documents",
            "program_link_candidates",
            "program_rule_extraction_failures",
            "push_tokens",
            "refresh_tokens",
            "storage_size_history",
        ];
        /// The file without its inline `#[cfg(test)] mod` blocks (braces
        /// counted; this test's own pattern spells its brace as \x7b).
        fn without_tests(source: &str) -> String {
            let marker = regex::Regex::new(r"#\[cfg\(test\)\]\s*\n\s*mod [a-z_0-9]+ \x7b").unwrap();
            let mut out = String::new();
            let mut at = 0;
            while let Some(found) = marker.find_at(source, at) {
                out.push_str(&source[at..found.start()]);
                let mut depth = 1;
                let mut end = found.end();
                for (offset, ch) in source[found.end()..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        end = found.end() + offset + 1;
                        break;
                    }
                }
                at = end;
            }
            out.push_str(&source[at..]);
            out
        }
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.file_name().unwrap().to_string_lossy().contains("test")
                {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let delete = regex::Regex::new(r"(?i)DELETE\s+FROM\s+([a-z_]+)").unwrap();
        let mut unreviewed = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for file in &files {
            let source = without_tests(&std::fs::read_to_string(file).unwrap());
            for found in delete.captures_iter(&source) {
                let table = found[1].to_string();
                if !REVIEWED.contains(&table.as_str()) {
                    unreviewed.push(format!("{} {}", file.display(), table));
                }
                seen.insert(table);
            }
        }
        assert!(unreviewed.is_empty(), "unreviewed deletes: {unreviewed:?}");
        // The list holds nothing stale.
        let stale: Vec<&&str> = REVIEWED.iter().filter(|t| !seen.contains(**t)).collect();
        assert!(stale.is_empty(), "no longer deleted: {stale:?}");
    }

    /// User data and the catalogue are never retention targets, nor is any
    /// name that is not an exact allowlist entry.
    #[tokio::test]
    async fn retention_refuses_every_table_outside_the_allowlist() {
        let pool = test_pool().await;
        for table in [
            "users",
            "user_profiles",
            "user_documents",
            "user_bookmarks",
            "user_program_states",
            "user_program_state_history",
            "application_journeys",
            "application_submissions",
            "application_preparations",
            "alert_subscriptions",
            "programs",
            "source_snapshots",
            "ingestion_runs",
            "operational_events",
            "INGESTION_ITEMS",
            "ingestion_items; DROP TABLE users",
            "public.ingestion_items",
            "",
        ] {
            let error = prune(&pool, table, 14, 1_000).await.unwrap_err();
            assert!(
                error.to_string().contains("retention refused"),
                "{table}: {error}"
            );
        }
        assert!(prune(&pool, "ingestion_items", 6, 1_000)
            .await
            .unwrap_err()
            .to_string()
            .contains("minimum"));
        assert_eq!(
            RETENTION_ALLOWLIST,
            &["ingestion_items", "corporate_door_reads"]
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn retention_deletes_only_items_of_old_runs_and_keeps_programmes_and_runs() {
        let pool = test_pool().await;
        let source = format!("retention_test_{}", Uuid::new_v4().simple());
        let program: Uuid = sqlx::query_scalar(
            "INSERT INTO programs(program_type,source_type,source_id,title) VALUES('welfare',$1,'p','보존 테스트') RETURNING id",
        )
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut runs = Vec::new();
        for age in ["30 days", "15 days", "2 days"] {
            let run: Uuid = sqlx::query_scalar(&format!(
                "INSERT INTO ingestion_runs(source_name,status,started_at) VALUES($1,'success',now()-interval '{age}') RETURNING id"
            ))
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
            for n in 0..3 {
                sqlx::query("INSERT INTO ingestion_items(run_id,source_id,program_id,status) VALUES($1,$2,$3,'unchanged')")
                    .bind(run)
                    .bind(format!("s{n}"))
                    .bind(program)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            runs.push(run);
        }
        let summary = prune(&pool, "ingestion_items", 14, 1_000).await.unwrap();
        assert!(summary.deleted >= 6, "{summary:?}");
        assert!(summary.vacuumed);
        let left: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT r.id, count(i.id) FROM ingestion_runs r LEFT JOIN ingestion_items i ON i.run_id=r.id
             WHERE r.id = ANY($1) GROUP BY r.id",
        )
        .bind(&runs)
        .fetch_all(&pool)
        .await
        .unwrap();
        let count = |run: Uuid| left.iter().find(|(id, _)| *id == run).unwrap().1;
        assert_eq!(left.len(), 3, "run summaries are kept");
        assert_eq!((count(runs[0]), count(runs[1]), count(runs[2])), (0, 0, 3));
        let program_kept: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM programs WHERE id=$1)")
                .bind(program)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(program_kept);

        sqlx::query("DELETE FROM ingestion_runs WHERE source_name=$1")
            .bind(&source)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(program)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn size_guard_levels_and_retention_tightening() {
        assert_eq!(size_level(419.9, 420, 470), None);
        assert_eq!(size_level(420.0, 420, 470), Some(Severity::Warning));
        assert_eq!(size_level(470.0, 420, 470), Some(Severity::Error));
        assert_eq!(effective_retention_days(14, 400.0, 420), (14, false));
        assert_eq!(effective_retention_days(14, 445.0, 420), (7, true));
        assert_eq!(effective_retention_days(7, 445.0, 420), (7, false));
    }

    #[test]
    fn df_output_is_parsed_in_gigabytes() {
        let text = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    overlay 65536000 41943040 24117248 64% /\n";
        assert_eq!(parse_df_available_gb(text), Some(23.0));
        assert_eq!(parse_df_available_gb("garbage"), None);
    }

    /// Above the warn threshold the job tightens retention to the minimum,
    /// cleans up at once, and records the automatic action and the size
    /// incident; below it the size incident resolves.
    #[tokio::test]
    async fn daily_storage_job_tightens_retention_and_records_the_actions() {
        let pool = test_pool().await;
        let config = StorageConfig {
            retention_enabled: true,
            retention_days: 14,
            batch_size: 1_000,
            db_warn_mb: 1,
            db_error_mb: 1_000_000,
            db_limit_mb: 1_000_000,
            disk_warn_gb: 2,
            disk_error_gb: 1,
        };
        let source = format!("retention_heal_{}", Uuid::new_v4().simple());
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_runs(source_name,status,started_at) VALUES($1,'success',now()-interval '10 days') RETURNING id",
        )
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ingestion_items(run_id,source_id,status) VALUES($1,'x','unchanged')",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();

        let details = run_daily(&pool, &config).await.unwrap();
        assert_eq!(details["retention_tightened"], true, "{details}");
        assert_eq!(details["retention"]["retention_days"], 7);
        assert_eq!(details["db_level"], "warning");
        let items: i64 = sqlx::query_scalar("SELECT count(*) FROM ingestion_items WHERE run_id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(items, 0, "a 10-day-old item goes under the 7-day minimum");
        let healing: (String, String) = sqlx::query_as(
            "SELECT status, component FROM operational_events
             WHERE fingerprint='self_healing:ingestion_retention_tightened'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(healing, ("resolved".into(), "self_healing".into()));
        // A second pass the same day adds no second self-healing record.
        let occurrences = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i32>(
                    "SELECT occurrence_count FROM operational_events
                     WHERE fingerprint='self_healing:ingestion_retention_tightened'",
                )
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let before = occurrences().await;
        run_daily(&pool, &config).await.unwrap();
        assert_eq!(occurrences().await, before);
        let size_status: String = sqlx::query_scalar(
            "SELECT status FROM operational_events WHERE fingerprint='storage:db_size'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(size_status, "open");

        let relaxed = StorageConfig {
            db_warn_mb: 999_999,
            ..config
        };
        let details = run_daily(&pool, &relaxed).await.unwrap();
        assert_eq!(details["retention_tightened"], false);
        let size_status: String = sqlx::query_scalar(
            "SELECT status FROM operational_events WHERE fingerprint='storage:db_size'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(size_status, "resolved");

        // The off switch keeps old rows and still measures.
        sqlx::query(
            "INSERT INTO ingestion_items(run_id,source_id,status) VALUES($1,'y','unchanged')",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
        let off = StorageConfig {
            retention_enabled: false,
            db_warn_mb: 1,
            ..config
        };
        let details = run_daily(&pool, &off).await.unwrap();
        assert_eq!(details["retention"], Value::Null);
        assert_eq!(details["retention_tightened"], false);
        assert!(details["database_mb"].as_f64().unwrap() > 0.0);
        let items: i64 = sqlx::query_scalar("SELECT count(*) FROM ingestion_items WHERE run_id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(items, 1);
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// The daily cap stops after the run that crosses it; the rest waits for
    /// the next day. Old runs go first.
    #[tokio::test]
    async fn retention_stops_at_the_daily_row_cap_oldest_runs_first() {
        let pool = test_pool().await;
        let source = format!("retention_cap_{}", Uuid::new_v4().simple());
        let mut runs = Vec::new();
        for age in ["400 days", "399 days", "398 days"] {
            let run: Uuid = sqlx::query_scalar(&format!(
                "INSERT INTO ingestion_runs(source_name,status,started_at) VALUES($1,'success',now()-interval '{age}') RETURNING id"
            ))
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
            for n in 0..2 {
                sqlx::query("INSERT INTO ingestion_items(run_id,source_id,status) VALUES($1,$2,'unchanged')")
                    .bind(run)
                    .bind(format!("s{n}"))
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            runs.push(run);
        }
        let summary = prune(&pool, "ingestion_items", 14, 1).await.unwrap();
        assert_eq!(
            (summary.runs_cleared, summary.deleted),
            (1, 2),
            "{summary:?}"
        );
        let left: Vec<i64> = sqlx::query_scalar(
            "SELECT count(i.id) FROM ingestion_runs r LEFT JOIN ingestion_items i ON i.run_id=r.id
             WHERE r.source_name=$1 GROUP BY r.id, r.started_at ORDER BY r.started_at",
        )
        .bind(&source)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(left, vec![0, 2, 2]);
        sqlx::query("DELETE FROM ingestion_runs WHERE source_name=$1")
            .bind(&source)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn reindex_follows_only_a_large_cleanup() {
        assert!(!should_reindex(0, 0));
        assert!(!should_reindex(0, 1_000_000));
        assert!(should_reindex(REINDEX_MIN_DELETED_ROWS, 10_000_000));
        assert!(!should_reindex(49_999, 1_000_000));
        // A tenth of the table as it was before the cleanup.
        assert!(should_reindex(100, 900));
        assert!(!should_reindex(99, 901));
        assert!(should_reindex(1, 0));
        assert!(should_reindex(1, -1), "a negative planner estimate is zero");
    }

    #[test]
    fn index_names_are_spliced_only_when_plain_and_leftovers_are_recognised() {
        for ok in ["idx_ingestion_items_run", "ingestion_items_pkey", "_x1"] {
            assert!(safe_index_name(ok), "{ok}");
        }
        for bad in [
            "",
            "Idx",
            "idx\"; DROP TABLE users; --",
            "idx-run",
            "1idx",
            "인덱스",
            &"a".repeat(64),
        ] {
            assert!(!safe_index_name(bad), "{bad}");
        }
        assert!(leftover_reindex_index("idx_ingestion_items_run_ccnew"));
        assert!(leftover_reindex_index("idx_ingestion_items_run_ccnew12"));
        assert!(!leftover_reindex_index("idx_ingestion_items_run"));
        assert!(!leftover_reindex_index("idx_ccnew_items"));
        assert!(!leftover_reindex_index("idx_ccnewx"));
        assert!(leftover_reindex_index("idx_ingestion_items_run_ccold"));
        assert!(!leftover_reindex_index("idx_ccoldy"));
    }

    /// 10-19: the database was 458 MB and the (run_id, source_id) unique
    /// index 110 MB, mostly dead pages after 254,934 deletions. A full
    /// rebuild took the indexes from 188 MB to about 43 MB. Judged by the
    /// old size the big index would be skipped (458 + 110 > 500); judged by
    /// its estimated rebuilt size it fits, and it is recognised as bloated.
    #[test]
    fn the_10_19_bloated_index_is_rebuilt_not_skipped() {
        let mb = 1_048_576_i64;
        // About 450k live rows; uuid (16) + short text (~20) per key.
        let (rows, width) = (450_000.0, Some(36.0));
        let estimate = rebuilt_size_estimate(110 * mb, rows, width, 109 * mb);
        assert!(estimate < 40 * mb, "{}", estimate / mb);
        assert!(estimate > 20 * mb, "{}", estimate / mb);
        assert!(has_headroom(458 * mb, estimate, 500 * mb));
        assert!(
            !has_headroom(458 * mb, 110 * mb, 500 * mb),
            "the old rule skipped it"
        );
        assert!(is_bloated(110 * mb, rows, width));
        // A packed index of the same rows is not bloated, nor is a small
        // one, nor one without statistics.
        assert!(!is_bloated(30 * mb, rows, width));
        assert!(!is_bloated(4 * mb, 1_000.0, width));
        assert!(!is_bloated(110 * mb, rows, None));
        // Without statistics: the current size, capped at the heap.
        assert_eq!(
            rebuilt_size_estimate(110 * mb, rows, None, 109 * mb),
            109 * mb
        );
        assert_eq!(rebuilt_size_estimate(5 * mb, rows, None, 109 * mb), 5 * mb);
        // The estimate never exceeds the index it replaces.
        assert_eq!(rebuilt_size_estimate(mb, rows, width, 109 * mb), mb);
        // Still refused when even the packed copy does not fit.
        assert!(!has_headroom(480 * mb, estimate, 500 * mb));
    }

    #[test]
    fn headroom_keeps_the_rebuild_under_the_hard_limit() {
        let mb = 1_048_576;
        assert!(has_headroom(313 * mb, 110 * mb, 500 * mb));
        assert!(!has_headroom(458 * mb, 110 * mb, 500 * mb));
        assert!(
            !has_headroom(390 * mb, 110 * mb, 500 * mb),
            "exactly at the limit"
        );
        assert!(!has_headroom(i64::MAX, 1, 500 * mb));
    }

    #[test]
    fn growth_projection_from_size_history() {
        let day = 86_400.0;
        let mb = 1_048_576.0;
        // 2 MB a day, last reading 400 MB, limit 500 MB: 50 days left.
        let points: Vec<(f64, f64)> = (0..=10)
            .map(|d| (d as f64 * day, (380.0 + 2.0 * d as f64) * mb))
            .collect();
        let p = project(&points, 500.0 * mb).unwrap();
        assert_eq!(p.growth_mb_per_day, 2.0);
        assert_eq!(p.days_to_limit, Some(50.0));
        assert_eq!((p.samples, p.span_days), (11, 10.0));
        // Shrinking or flat: no projected date.
        let shrinking = [(0.0, 450.0 * mb), (3.0 * day, 313.0 * mb)];
        let p = project(&shrinking, 500.0 * mb).unwrap();
        assert!(p.growth_mb_per_day < 0.0 && p.days_to_limit.is_none());
        // Too little history to say anything.
        assert_eq!(project(&[], 500.0 * mb), None);
        assert_eq!(project(&[(0.0, mb)], 500.0 * mb), None);
        assert_eq!(project(&[(0.0, mb), (3_600.0, 2.0 * mb)], 500.0 * mb), None);
        // Already past the limit: zero days, never negative.
        let over = [(0.0, 499.0 * mb), (day, 501.0 * mb)];
        assert_eq!(project(&over, 500.0 * mb).unwrap().days_to_limit, Some(0.0));
    }

    #[test]
    fn healing_message_states_the_measured_sizes() {
        let shrank = healing_message(254_934, 4, 458.0, 313.0);
        assert!(shrank.contains("458MB → 313MB"), "{shrank}");
        assert!(shrank.contains("인덱스 4개"), "{shrank}");
        assert!(!shrank.contains("줄지 않았"), "{shrank}");
        let flat = healing_message(254_934, 0, 458.0, 458.0);
        assert!(flat.contains("줄지 않았습니다: 458MB → 458MB"), "{flat}");
    }

    async fn index_files(pool: &PgPool) -> Vec<(String, i64)> {
        sqlx::query_as(
            "SELECT c.relname::text, c.relfilenode::bigint FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
             WHERE i.indrelid='public.ingestion_items'::regclass ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Rebuilds run only on the allowlisted table, one index at a time, and
    /// each one really produces a new index file; without headroom nothing
    /// is rebuilt; a leftover `_ccnew` index of a cut-off rebuild is dropped
    /// and any other invalid index is left alone.
    #[tokio::test]
    async fn reindex_rebuilds_allowlisted_indexes_within_headroom_only() {
        let pool = test_pool().await;
        for table in [
            "users",
            "programs",
            "storage_size_history",
            "ingestion_runs",
        ] {
            let error = reindex(&pool, table, i64::MAX, true, Instant::now())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("retention refused"), "{table}");
        }
        let users_before: Vec<i64> = sqlx::query_scalar(
            "SELECT c.relfilenode::bigint FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
             WHERE i.indrelid='public.users'::regclass ORDER BY c.relname",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        // No headroom (limit of one byte): nothing is rebuilt.
        let before = index_files(&pool).await;
        let summary = reindex(&pool, "ingestion_items", 1, true, Instant::now())
            .await
            .unwrap();
        assert_eq!(summary.rebuilt, 0);
        assert!(
            summary
                .indexes
                .iter()
                .all(|r| r.outcome == "skipped_headroom"),
            "{summary:?}"
        );
        assert_eq!(index_files(&pool).await, before);

        // A leftover of a failed concurrent build, and an unrelated invalid
        // index: a unique build over duplicate values fails and stays invalid.
        let source = format!("reindex_{}", uuid::Uuid::new_v4().simple());
        let run: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_runs(source_name,status) VALUES($1,'success') RETURNING id",
        )
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
        for n in 0..2 {
            sqlx::query(
                "INSERT INTO ingestion_items(run_id,source_id,status) VALUES($1,$2,'unchanged')",
            )
            .bind(run)
            .bind(format!("dup{n}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        for name in ["idx_ingestion_items_run_ccnew", "test_invalid_status"] {
            let _ = sqlx::query(&format!(
                "CREATE UNIQUE INDEX CONCURRENTLY {name} ON ingestion_items(status)"
            ))
            .execute(&pool)
            .await
            .unwrap_err();
        }
        let invalid: Vec<String> = sqlx::query_scalar(
            "SELECT c.relname::text FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
             WHERE i.indrelid='public.ingestion_items'::regclass AND NOT i.indisvalid ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            invalid,
            vec!["idx_ingestion_items_run_ccnew", "test_invalid_status"]
        );

        let summary = reindex(&pool, "ingestion_items", i64::MAX, true, Instant::now())
            .await
            .unwrap();
        let outcome = |name: &str| {
            summary
                .indexes
                .iter()
                .find(|r| r.index == name)
                .map(|r| r.outcome)
        };
        assert_eq!(
            outcome("idx_ingestion_items_run_ccnew"),
            Some("dropped_leftover")
        );
        assert_eq!(outcome("test_invalid_status"), Some("skipped_invalid"));
        assert_eq!(summary.rebuilt, before.len(), "{summary:?}");
        let after = index_files(&pool).await;
        for (name, file) in &before {
            let (_, new_file) = after.iter().find(|(n, _)| n == name).expect(name);
            assert_ne!(new_file, file, "{name} was rebuilt into a new file");
        }
        assert!(!after
            .iter()
            .any(|(n, _)| n == "idx_ingestion_items_run_ccnew"));
        assert!(after.iter().any(|(n, _)| n == "test_invalid_status"));
        let users_after: Vec<i64> = sqlx::query_scalar(
            "SELECT c.relfilenode::bigint FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
             WHERE i.indrelid='public.users'::regclass ORDER BY c.relname",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(users_after, users_before, "other tables are never touched");

        sqlx::query("DROP INDEX test_invalid_status")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// The daily job rebuilds indexes after a large cleanup, writes one
    /// history row per run, keeps 400 days of history, projects the days to
    /// the limit into the size event, and its self-healing message reports
    /// the measured sizes.
    #[tokio::test]
    async fn daily_job_reindexes_records_history_and_projects_days_to_limit() {
        let pool = test_pool().await;
        // Readings left by earlier test runs on this database would skew the
        // fit; the seeds below are the whole recent history.
        sqlx::query(
            "DELETE FROM storage_size_history WHERE measured_at > now() - interval '31 days'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mb = 1_048_576_i64;
        let mut seeded = vec![];
        // Fixed small seeds: any real database (several MB after migrations)
        // reads above them, so the fit is growth. The run may itself shrink
        // the database (VACUUM truncates an empty tail), so the seeds are not
        // relative to the starting size.
        for (age, bytes) in [
            ("401 days", 1_i64),
            ("399 days", 2),
            ("20 days", mb),
            ("10 days", 2 * mb),
        ] {
            let id: i64 = sqlx::query_scalar(&format!(
                "INSERT INTO storage_size_history(measured_at,db_bytes) VALUES(now()-interval '{age}',$1) RETURNING id"
            ))
            .bind(bytes)
            .fetch_one(&pool)
            .await
            .unwrap();
            seeded.push(id);
        }
        let source = format!("reindex_daily_{}", uuid::Uuid::new_v4().simple());
        let run: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_runs(source_name,status,started_at) VALUES($1,'success',now()-interval '30 days') RETURNING id",
        )
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO ingestion_items(run_id,source_id,status) SELECT $1, 's'||g, 'unchanged' FROM generate_series(1,50) g")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        let rows_before: i64 = sqlx::query_scalar("SELECT count(*) FROM storage_size_history")
            .fetch_one(&pool)
            .await
            .unwrap();
        let config = StorageConfig {
            retention_enabled: true,
            retention_days: 14,
            batch_size: 1_000,
            db_warn_mb: 1,
            db_error_mb: 1_000_000,
            db_limit_mb: 1_000_000,
            disk_warn_gb: 2,
            disk_error_gb: 1,
        };
        let details = run_daily(&pool, &config).await.unwrap();
        assert!(
            details["retention"]["deleted"].as_u64().unwrap() >= 50,
            "{details}"
        );
        assert!(
            details["reindex"]["rebuilt"].as_u64().unwrap() >= 1,
            "{details}"
        );
        let kept: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM storage_size_history WHERE id = ANY($1) ORDER BY id",
        )
        .bind(&seeded)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            kept,
            seeded[1..].to_vec(),
            "only the 401-day reading is dropped"
        );
        let rows_after: i64 = sqlx::query_scalar("SELECT count(*) FROM storage_size_history")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            rows_after, rows_before,
            "one reading in, one expired reading out"
        );
        // The projection is the fit over the stored readings (the seeds and
        // this run's own); `project` itself is checked in its unit test.
        let points: Vec<(f64, f64)> = sqlx::query_as(
            "SELECT extract(epoch FROM measured_at)::float8, db_bytes::float8 FROM storage_size_history
             WHERE measured_at > now() - interval '30 days' ORDER BY measured_at",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(points.len(), 3, "two seeds and this run's reading");
        let expected = project(&points, (1_000_000 * mb) as f64).unwrap();
        assert_eq!(
            details["projection"],
            serde_json::to_value(expected).unwrap()
        );
        let growth = details["projection"]["growth_mb_per_day"].as_f64().unwrap();
        assert!(growth > 0.0, "seeded at 1 MB and 2 MB: {details}");
        let event: Value = sqlx::query_scalar(
            "SELECT details FROM operational_events WHERE fingerprint='storage:db_size'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            event["projected_days_to_limit"].as_f64().unwrap() > 0.0,
            "{event}"
        );
        assert_eq!(event["limit_mb"], 1_000_000);
        let message: String = sqlx::query_scalar(
            "SELECT message FROM operational_events WHERE fingerprint='self_healing:ingestion_retention_tightened'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(message.contains("측정한 DB 용량"), "{message}");
        assert!(message.contains("MB →"), "{message}");

        sqlx::query("DELETE FROM storage_size_history WHERE id = ANY($1)")
            .bind(&seeded)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// Bloat drives the rebuild on any run, without a large cleanup, so a
    /// skipped or failed rebuild is retried the next day; once rebuilt the
    /// index is no longer bloated and is left alone. The packed-size
    /// estimate matches a freshly built index. Each rebuild runs on its own
    /// connection with a longer statement_timeout that is reset afterwards.
    #[tokio::test]
    async fn bloated_indexes_are_rebuilt_on_any_run_on_a_reset_connection() {
        let admin = test_pool().await;
        // A role-style default like production's (2 minutes) for new
        // sessions, on whatever database the test URL names (identifier
        // quoted by format('%I'), never spliced from Rust).
        sqlx::query(
            "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I SET statement_timeout = %L', current_database(), '2min'); END $$",
        )
            .execute(&admin)
            .await
            .unwrap();
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        // One connection: the rebuild and the checks share it.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let source = format!("bloat_{}", uuid::Uuid::new_v4().simple());
        let mut runs = vec![];
        for rows in [250_000, 20_000] {
            let run: uuid::Uuid = sqlx::query_scalar(
                "INSERT INTO ingestion_runs(source_name,status) VALUES($1,'success') RETURNING id",
            )
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO ingestion_items(run_id,source_id,status) SELECT $1, 's'||g, 'unchanged' FROM generate_series(1,$2) g")
                .bind(run)
                .bind(rows)
                .execute(&pool)
                .await
                .unwrap();
            runs.push(run);
        }
        sqlx::query("DELETE FROM ingestion_items WHERE run_id=$1")
            .bind(runs[0])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("VACUUM (ANALYZE) ingestion_items")
            .execute(&pool)
            .await
            .unwrap();
        const UNIQUE: &str = "ingestion_items_run_id_source_id_key";
        let summary = reindex(&pool, "ingestion_items", i64::MAX, false, Instant::now())
            .await
            .unwrap();
        let unique = summary
            .indexes
            .iter()
            .find(|r| r.index == UNIQUE)
            .unwrap_or_else(|| panic!("{summary:?}"));
        assert!(
            unique.bloated && unique.outcome == "reindexed",
            "{unique:?}"
        );
        assert!(summary.indexes.iter().all(|r| r.bloated), "{summary:?}");
        let after = unique.bytes_after.unwrap();
        assert!(after * 4 < unique.bytes_before, "{unique:?}");
        // The packed estimate (margin removed) is within 30% of the real
        // freshly built size.
        let packed = unique.estimated_rebuilt_bytes as f64 / REBUILD_ESTIMATE_MARGIN;
        let ratio = packed / after as f64;
        assert!(
            (0.7..=1.3).contains(&ratio),
            "estimate {packed} vs built {after}"
        );
        let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(timeout, "2min", "the dedicated connection was reset");
        // Nothing is bloated any more: the next run leaves the indexes alone.
        let again = reindex(&pool, "ingestion_items", i64::MAX, false, Instant::now())
            .await
            .unwrap();
        assert!(
            !again.indexes.iter().any(|r| r.index == UNIQUE),
            "{again:?}"
        );
        // The budget counts from the job start: a job that started long ago
        // starts no rebuild.
        let late = Instant::now() - REINDEX_BUDGET;
        let skipped = reindex(&pool, "ingestion_items", i64::MAX, true, late)
            .await
            .unwrap();
        assert_eq!(skipped.rebuilt, 0);
        assert!(
            skipped
                .indexes
                .iter()
                .filter(|r| r.outcome != "skipped_invalid")
                .all(|r| r.outcome == "skipped_budget"),
            "{skipped:?}"
        );

        sqlx::query("DELETE FROM ingestion_runs WHERE source_name=$1")
            .bind(&source)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        sqlx::query(
            "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I RESET statement_timeout', current_database()); END $$",
        )
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
