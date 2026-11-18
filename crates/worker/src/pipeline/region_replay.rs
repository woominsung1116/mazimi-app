//! Narrow, transactional upgrades for explicitly reviewed region-only versions.
//! Older versions and mismatched source payloads require the full replay path.
//! Payloads are compared without their view counters
//! (`ingestion_stable_payload`): a programme whose stored payload only lags
//! the snapshot's counter is the same source record.
use super::{normalizer_version, RenormalizationSummary, SourceRenormalizationSummary};
use crate::{error_safety::persisted_error, regions};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

const BATCH_SIZE: usize = 200;
/// Reviewed region-only transitions. A later version of a source (youth_center
/// v5, scholarship v3 and gov_benefits v5 changed application windows, not
/// only regions; local_welfare v5 adds `region_districts`, which this path
/// does not write) retires its entry: those snapshots go through the full
/// replay path. Empty until a new region-only version is reviewed.
const REGION_UPGRADES: &[(&str, i32, i32)] = &[];

#[derive(FromRow)]
struct StoredRegionRecord {
    snapshot_id: Uuid,
    program_id: Uuid,
    source_id: String,
    content_hash: String,
    raw_payload: Value,
}

fn mapped_regions(source: &str, payload: &Value) -> Vec<String> {
    match source {
        "gov_benefits" => regions::government(payload),
        "scholarship" => regions::scholarship(payload),
        "youth_center" => regions::youth(payload),
        "local_welfare" => regions::local_welfare_scope(payload).regions,
        _ => unreachable!("only reviewed source transitions are dispatched"),
    }
}

const SELECT_BATCH: &str = "SELECT s.id AS snapshot_id,p.id AS program_id,s.source_id,s.content_hash,s.raw_payload
    FROM source_snapshots s JOIN programs p ON p.source_type=s.source_name AND p.source_id=s.source_id
    WHERE s.source_name=$1 AND s.normalizer_version=$2 AND p.normalizer_version=$2
      AND ingestion_stable_payload(s.source_name,s.raw_payload)=ingestion_stable_payload(p.source_type,p.raw_payload)
      AND jsonb_typeof(p.normalized_payload)='object'
    ORDER BY s.fetched_at,s.source_id LIMIT $3 FOR UPDATE OF s,p SKIP LOCKED";

/// Upgrade only the reviewed region-only transitions. A batch locks both
/// representations, computes regions locally, then changes data and audit rows
/// in the same transaction. No upstream requests or application writes occur.
/// `--regions-only` never stands in for a general multi-version replay.
pub async fn renormalize_stored_regions(
    pool: &PgPool,
    source_filter: Option<&str>,
    limit: usize,
) -> Result<RenormalizationSummary> {
    anyhow::ensure!(
        (1..=100_000).contains(&limit),
        "renormalization limit must be between 1 and 100000"
    );
    let filter = source_filter.map(str::trim);
    anyhow::ensure!(
        !REGION_UPGRADES.is_empty(),
        "no reviewed region-only upgrade; use full replay"
    );
    anyhow::ensure!(
        filter.is_none_or(|source| REGION_UPGRADES.iter().any(|(name, _, _)| *name == source)),
        "regions-only has no reviewed upgrade for this source; use full replay"
    );
    let mut summary = RenormalizationSummary::default();
    for &(source, previous, current) in REGION_UPGRADES {
        if filter.is_some_and(|name| name != source) || summary.selected >= limit {
            continue;
        }
        // An unrelated future version must explicitly implement/review its own
        // upgrade; never automatically infer that only regions have changed.
        anyhow::ensure!(
            normalizer_version(source) == current,
            "reviewed region upgrade is no longer current for {source}; use full replay"
        );
        let run_id = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,normalizer_version,run_kind,total_fetched,total_changed) VALUES($1,$2,'running',now(),$3,'renormalization',0,0)")
            .bind(run_id).bind(source).bind(current).execute(pool).await?;
        let result = run_batches(
            pool,
            source,
            previous,
            current,
            run_id,
            limit - summary.selected,
        )
        .await;
        match result {
            Ok(row) => {
                sqlx::query(
                    "UPDATE ingestion_runs SET status='success',finished_at=now() WHERE id=$1",
                )
                .bind(run_id)
                .execute(pool)
                .await?;
                summary.selected += row.selected;
                summary.renormalized += row.renormalized;
                summary.skipped += row.skipped;
                summary.sources.push(row);
            }
            Err(error) => {
                // Completed batches retain exact progress/items even if the
                // next batch fails. Its partial writes have been rolled back.
                sqlx::query("UPDATE ingestion_runs SET status='failed',finished_at=now(),error_message=$2 WHERE id=$1")
                    .bind(run_id).bind(persisted_error(&error)).execute(pool).await?;
                return Err(
                    error.context("region-only replay stopped; completed batches are preserved")
                );
            }
        }
    }
    Ok(summary)
}

async fn run_batches(
    pool: &PgPool,
    source: &str,
    previous: i32,
    current: i32,
    run_id: Uuid,
    limit: usize,
) -> Result<SourceRenormalizationSummary> {
    let mut stats = SourceRenormalizationSummary {
        source_name: source.into(),
        normalizer_version: current,
        selected: 0,
        renormalized: 0,
        failed: 0,
        quarantined: 0,
        skipped: 0,
    };
    while stats.selected < limit {
        let mut tx = pool.begin().await?;
        let records: Vec<StoredRegionRecord> = sqlx::query_as(SELECT_BATCH)
            .bind(source)
            .bind(previous)
            .bind((limit - stats.selected).min(BATCH_SIZE) as i64)
            .fetch_all(&mut *tx)
            .await?;
        if records.is_empty() {
            tx.rollback().await?;
            break;
        }
        let patch:Vec<Value>=records.iter().map(|row| json!({
            "snapshot_id":row.snapshot_id,"program_id":row.program_id,"source_id":row.source_id,
            "content_hash":row.content_hash,"raw_payload":row.raw_payload,
            "regions":mapped_regions(source,&row.raw_payload)
        })).collect();
        let updated = apply_batch(&mut tx, source, previous, current, run_id, &patch).await?;
        sqlx::query("UPDATE ingestion_runs SET total_fetched=COALESCE(total_fetched,0)+$2,total_renormalized=total_renormalized+$3 WHERE id=$1")
            .bind(run_id).bind(records.len() as i32).bind(updated as i32).execute(&mut *tx).await?;
        tx.commit().await?;
        stats.selected += records.len();
        stats.renormalized += updated;
        stats.skipped += records.len() - updated;
        tracing::info!(
            source,
            selected = stats.selected,
            renormalized = stats.renormalized,
            skipped = stats.skipped,
            "region-only replay batch committed"
        );
    }
    Ok(stats)
}

async fn apply_batch(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    previous: i32,
    current: i32,
    run_id: Uuid,
    patch: &[Value],
) -> Result<usize> {
    anyhow::ensure!(
        !patch.is_empty() && patch.len() <= BATCH_SIZE,
        "region replay batch must contain 1..200 records"
    );
    let updated:i64=sqlx::query_scalar(
      "WITH input AS MATERIALIZED (
         SELECT * FROM jsonb_to_recordset($1) AS r(snapshot_id uuid,program_id uuid,source_id text,content_hash text,raw_payload jsonb,regions jsonb)
       ), eligible AS MATERIALIZED (
         SELECT i.* FROM input i
         JOIN source_snapshots s ON s.id=i.snapshot_id AND s.source_name=$2 AND s.source_id=i.source_id
         JOIN programs p ON p.id=i.program_id AND p.source_type=$2 AND p.source_id=i.source_id
         WHERE s.normalizer_version=$3 AND p.normalizer_version=$3
           AND s.content_hash=i.content_hash AND s.raw_payload=i.raw_payload
           AND ingestion_stable_payload($2,p.raw_payload)=ingestion_stable_payload($2,i.raw_payload)
           AND jsonb_typeof(p.normalized_payload)='object'
       ), programs_updated AS (
         UPDATE programs p SET regions=ARRAY(SELECT jsonb_array_elements_text(e.regions)),
           normalized_payload=jsonb_set(p.normalized_payload,'{regions}',e.regions,true),
           normalizer_version=$4,normalized_at=now(),updated_at=now()
         FROM eligible e WHERE p.id=e.program_id RETURNING p.id
       ), snapshots_updated AS (
         UPDATE source_snapshots s SET normalizer_version=$4,normalized_at=now()
         FROM eligible e JOIN programs_updated p ON p.id=e.program_id
         WHERE s.id=e.snapshot_id RETURNING s.id
       ), audit AS (
         INSERT INTO ingestion_items(id,run_id,source_id,program_id,status,error_message)
         SELECT gen_random_uuid(),$5,i.source_id,i.program_id,
            CASE WHEN s.id IS NOT NULL THEN 'renormalized' ELSE 'unchanged' END,
            CASE WHEN s.id IS NULL THEN 'Source payload or normalization version changed; full replay may be required' ELSE NULL END
         FROM input i LEFT JOIN snapshots_updated s ON s.id=i.snapshot_id
         ON CONFLICT(run_id,source_id) DO UPDATE SET status=excluded.status,program_id=excluded.program_id,error_message=excluded.error_message
         RETURNING status
       ) SELECT count(*) FROM audit WHERE status='renormalized'")
        .bind(json!(patch)).bind(source).bind(previous).bind(current).bind(run_id)
        .fetch_one(&mut **tx).await.context("atomic region replay batch failed")?;
    Ok(updated as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_only_versions_and_mappings_are_explicit() {
        // local_welfare v5 writes districts too: full replay only.
        assert!(REGION_UPGRADES.is_empty());
        for &(source, previous, current) in REGION_UPGRADES {
            assert_eq!(previous + 1, current);
            assert_eq!(normalizer_version(source), current);
        }
        assert_eq!(
            mapped_regions("youth_center", &json!({"zipCd":"41461"})),
            ["gyeonggi"]
        );
        assert_eq!(
            mapped_regions(
                "gov_benefits",
                &json!({"소관기관유형":"시군구","소관기관명":"부산광역시"})
            ),
            ["busan"]
        );
        assert_eq!(
            mapped_regions("local_welfare", &json!({"ctpvNm":"대구광역시"})),
            ["daegu"]
        );
        assert_eq!(
            mapped_regions("scholarship", &json!({"지역거주여부 상세내용":"전국"})),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn local_welfare_v5_is_not_a_region_only_upgrade() {
        // Refused before any query, so the pool never connects.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .unwrap();
        let error = renormalize_stored_regions(&pool, Some("local_welfare"), 10)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("use full replay"), "{error}");
    }

    /// Without `--source`, an empty upgrade list must not report success.
    #[tokio::test]
    async fn regions_only_without_a_reviewed_upgrade_is_refused() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .unwrap();
        let error = renormalize_stored_regions(&pool, None, 10)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("use full replay"), "{error}");
    }

    #[tokio::test]
    async fn atomic_region_replay_guards_versions_payloads_and_preserves_applications() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("isolated test DB required");
        let mut tx = pool.begin().await.unwrap();
        let run_id = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,normalizer_version,run_kind) VALUES($1,'youth_center',4,'renormalization')")
            .bind(run_id).execute(&mut *tx).await.unwrap();
        let raw = json!({"zipCd":"41461","plcyNm":"원문 공고"});
        let mut patch = Vec::new();
        let mut programs = Vec::new();
        let mut snapshots = Vec::new();
        for index in 0..8 {
            let program_id = Uuid::new_v4();
            let snapshot_id = Uuid::new_v4();
            let source_id = format!("region_replay_{}_{}", run_id.simple(), index);
            sqlx::query("INSERT INTO programs(id,source_type,source_id,program_type,title,program_status,is_active,raw_payload,normalized_payload,normalizer_version,application_method,regions) VALUES($1,'youth_center',$2,'welfare','보존할 공고','closed',false,$3,$4,3,'보존할 신청방법',ARRAY[]::text[])")
                .bind(program_id).bind(&source_id).bind(&raw).bind(json!({"title":"보존할 정규화","regions":[],"amount":100})).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO source_snapshots(id,source_name,source_id,content_hash,raw_payload,normalizer_version,fetched_at) VALUES($1,'youth_center',$2,'original-hash',$3,3,'1970-01-01')")
                .bind(snapshot_id).bind(&source_id).bind(&raw).execute(&mut *tx).await.unwrap();
            patch.push(json!({"program_id":program_id,"snapshot_id":snapshot_id,"source_id":source_id,"content_hash":"original-hash","raw_payload":raw,"regions":mapped_regions("youth_center",&raw)}));
            programs.push(program_id);
            snapshots.push(snapshot_id);
        }
        let user = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id) VALUES($1)")
            .bind(user)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO user_program_states(user_id,program_id,state) VALUES($1,$2,'applied')",
        )
        .bind(user)
        .bind(programs[0])
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO user_program_state_history(user_id,program_id,state,memo) VALUES($1,$2,'applied','접수 이력 보존')")
            .bind(user).bind(programs[0]).execute(&mut *tx).await.unwrap();
        let before: Value = sqlx::query_scalar(
            "SELECT program FROM application_program_snapshots WHERE user_id=$1 AND program_id=$2",
        )
        .bind(user)
        .bind(programs[0])
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        // Reproduce changes after a caller captured a source payload/hash. The
        // optimistic guards must reject each independently of row locking.
        sqlx::query("UPDATE source_snapshots SET raw_payload=$2 WHERE id=$1")
            .bind(snapshots[1])
            .bind(json!({"zipCd":"26110"}))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE source_snapshots SET content_hash='new-hash' WHERE id=$1")
            .bind(snapshots[2])
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE programs SET raw_payload=$2 WHERE id=$1")
            .bind(programs[3])
            .bind(json!({"zipCd":"27110"}))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE source_snapshots SET normalizer_version=2 WHERE id=$1")
            .bind(snapshots[4])
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE programs SET normalizer_version=2 WHERE id=$1")
            .bind(programs[5])
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE programs SET normalized_payload='[]' WHERE id=$1")
            .bind(programs[6])
            .execute(&mut *tx)
            .await
            .unwrap();
        // Only the view counter differs between the two representations: the
        // same source payload, so it is upgraded.
        sqlx::query(
            "UPDATE programs SET raw_payload=raw_payload||'{\"inqCnt\":\"12\"}' WHERE id=$1",
        )
        .bind(programs[7])
        .execute(&mut *tx)
        .await
        .unwrap();
        let selected: Vec<StoredRegionRecord> = sqlx::query_as(SELECT_BATCH)
            .bind("youth_center")
            .bind(3)
            .bind(BATCH_SIZE as i64)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert!(selected.len() <= 200);
        let selected_fixture_ids: Vec<_> = selected
            .iter()
            .filter(|row| programs.contains(&row.program_id))
            .map(|row| row.program_id)
            .collect();
        assert!(selected_fixture_ids.contains(&programs[0]));
        assert!(selected_fixture_ids.contains(&programs[2]));
        assert!(selected_fixture_ids.contains(&programs[7]));
        assert_eq!(
            selected_fixture_ids.len(),
            3,
            "only matching previous versions and source payloads may enter a batch"
        );
        assert_eq!(
            apply_batch(&mut tx, "youth_center", 3, 4, run_id, &patch)
                .await
                .unwrap(),
            2
        );
        let (title,status,active,method,version,normalized,region):(String,String,bool,String,i32,Value,Vec<String>)=
            sqlx::query_as("SELECT title,program_status,is_active,application_method,normalizer_version,normalized_payload,regions FROM programs WHERE id=$1")
                .bind(programs[0]).fetch_one(&mut *tx).await.unwrap();
        assert_eq!(
            (
                title.as_str(),
                status.as_str(),
                active,
                method.as_str(),
                version
            ),
            ("보존할 공고", "closed", false, "보존할 신청방법", 4)
        );
        assert_eq!(
            normalized,
            json!({"title":"보존할 정규화","regions":["gyeonggi"],"amount":100})
        );
        assert_eq!(region, ["gyeonggi"]);
        let snapshot_version: i32 =
            sqlx::query_scalar("SELECT normalizer_version FROM source_snapshots WHERE id=$1")
                .bind(snapshots[0])
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(snapshot_version, 4);
        let (application,state,history):(Value,String,i64)=sqlx::query_as("SELECT a.program,s.state,(SELECT count(*) FROM user_program_state_history h WHERE h.user_id=a.user_id AND h.program_id=a.program_id) FROM application_program_snapshots a JOIN user_program_states s USING(user_id,program_id) WHERE a.user_id=$1 AND a.program_id=$2")
            .bind(user).bind(programs[0]).fetch_one(&mut *tx).await.unwrap();
        assert_eq!(before, application);
        assert_eq!(state, "applied");
        assert_eq!(history, 1);
        let modified: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM programs WHERE id=ANY($1) AND cardinality(regions)>0",
        )
        .bind(&programs)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(modified, 2);
        let counts:(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE status='renormalized'),count(*) FILTER(WHERE status='unchanged') FROM ingestion_items WHERE run_id=$1")
            .bind(run_id).fetch_one(&mut *tx).await.unwrap();
        assert_eq!(counts, (2, 6));
        let second_run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,normalizer_version,run_kind) VALUES($1,'youth_center',4,'renormalization')")
            .bind(second_run).execute(&mut *tx).await.unwrap();
        assert_eq!(
            apply_batch(&mut tx, "youth_center", 3, 4, second_run, &patch[..1])
                .await
                .unwrap(),
            0
        );
        assert!(apply_batch(
            &mut tx,
            "youth_center",
            3,
            4,
            second_run,
            &vec![patch[0].clone(); 201]
        )
        .await
        .is_err());
        tx.rollback().await.unwrap();
        pool.close().await;
    }
}
