//! 보조금24 구비서류 lives in the serviceDetail row (`gov_benefits::DETAIL_KEY`),
//! side data outside the change hash. It must reach
//! `programs.submission_documents` on a changed and on an unchanged record,
//! never blank a stored text when the detail read failed, and never make a
//! record "changed" (no renormalization, no `updated_at`, no AI re-queue
//! beyond the column trigger, no ingestion change count).
use super::*;
use crate::sources::gov_benefits::DETAIL_KEY;
use serde_json::{json, Value};

async fn test_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("isolated test database is required")
}

fn list_row(source_id: &str, title: &str) -> Value {
    json!({"서비스ID": source_id, "서비스명": title, "신청기한": "상시신청", "조회수": 1})
}

fn with_detail(mut payload: Value, documents: Option<&str>) -> Value {
    let mut detail = serde_json::Map::new();
    detail.insert("접수기관명".into(), json!("행정복지센터"));
    if let Some(text) = documents {
        detail.insert("구비서류".into(), json!(text));
    }
    payload[DETAIL_KEY] = Value::Object(detail);
    payload
}

#[test]
fn the_normalizer_reads_documents_from_the_detail_row() {
    let list = list_row("SVC1", "청년 월세 지원");
    let norm = normalize_gov_benefits(&with_detail(
        list.clone(),
        Some("  ○ 신분증\n○ 통장사본 \n"),
    ));
    assert_eq!(
        norm.submission_documents.as_deref(),
        Some("○ 신분증\n○ 통장사본")
    );
    assert!(!norm.keep_stored_submission_documents);
    // A detail row without the text, or with a blank one: not read, the
    // stored text is kept (a one-run upstream gap never blanks it).
    for documents in [None, Some("  ")] {
        let norm = normalize_gov_benefits(&with_detail(list.clone(), documents));
        assert_eq!(norm.submission_documents, None);
        assert!(norm.keep_stored_submission_documents);
    }
    // No detail row (the detail read failed): keep what is stored.
    let norm = normalize_gov_benefits(&list);
    assert_eq!(norm.submission_documents, None);
    assert!(norm.keep_stored_submission_documents);
    // A top-level 구비서류 (never sent by serviceList) is not read.
    let mut top = list;
    top["구비서류"] = json!("신분증");
    assert_eq!(normalize_gov_benefits(&top).submission_documents, None);
}

struct Fixture {
    pool: PgPool,
    source_id: String,
    runs: Vec<Uuid>,
}

/// What a sync left on the row.
#[derive(Debug, PartialEq)]
struct Row {
    documents: Option<String>,
    updated_at: chrono::DateTime<Utc>,
    /// The row version: equal before and after means nothing was written.
    xmin: String,
    title: String,
}

impl Fixture {
    async fn new() -> Self {
        Self {
            pool: test_pool().await,
            source_id: format!("side-docs-{}", Uuid::new_v4().simple()),
            runs: Vec::new(),
        }
    }

    /// One fresh fetch; returns the outcome and the ingestion_items status.
    async fn fetch(&mut self, payload: Value) -> (ProcessOutcome, String) {
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'gov_benefits','running',now(),'ingestion')")
            .bind(run).execute(&self.pool).await.unwrap();
        self.runs.push(run);
        let record = RawRecord {
            source_id: self.source_id.clone(),
            content_hash: crate::sources::content_hash(&payload),
            payload,
        };
        let outcome = process_record(
            &self.pool,
            "gov_benefits",
            run,
            &record,
            &DedupCaches::default(),
            &HashSet::new(),
            RecordOrigin::FreshFetch,
        )
        .await
        .unwrap();
        let status = sqlx::query_scalar(
            "SELECT status FROM ingestion_items WHERE run_id=$1 AND source_id=$2",
        )
        .bind(run)
        .bind(&self.source_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (outcome, status)
    }

    async fn row(&self) -> Row {
        let (documents, updated_at, xmin, title) = sqlx::query_as::<_, (Option<String>, chrono::DateTime<Utc>, String, String)>(
            "SELECT submission_documents, updated_at, xmin::text, title FROM programs WHERE source_type='gov_benefits' AND source_id=$1",
        )
        .bind(&self.source_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        Row {
            documents,
            updated_at,
            xmin,
            title,
        }
    }

    async fn cleanup(self) {
        let pool = &self.pool;
        sqlx::query("DELETE FROM ingestion_items WHERE run_id=ANY($1)")
            .bind(&self.runs)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=ANY($1)")
            .bind(&self.runs)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE source_type='gov_benefits' AND source_id=$1")
            .bind(&self.source_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='gov_benefits' AND source_id=$1",
        )
        .bind(&self.source_id)
        .execute(pool)
        .await
        .unwrap();
        pool.close().await;
    }
}

#[tokio::test]
async fn documents_follow_the_detail_row_without_a_content_change() {
    let mut fx = Fixture::new().await;
    let list = list_row(&fx.source_id, "청년 월세 지원");

    // A row stored by the earlier mapping: no detail, no documents.
    assert_eq!(fx.fetch(list.clone()).await.1, "upserted");
    let before = fx.row().await;
    assert_eq!(before.documents, None);

    // The next run carries the detail row: the text is filled in, and the
    // record is still unchanged (same hash, no renormalization, no
    // updated_at).
    let (outcome, status) = fx
        .fetch(with_detail(list.clone(), Some("○ 신분증\n○ 통장사본")))
        .await;
    assert_eq!(
        (outcome, status.as_str()),
        (ProcessOutcome::Unchanged, "unchanged")
    );
    let filled = fx.row().await;
    assert_eq!(filled.documents.as_deref(), Some("○ 신분증\n○ 통장사본"));
    assert_eq!(filled.updated_at, before.updated_at);

    // Idempotent: the same text again writes nothing.
    fx.fetch(with_detail(list.clone(), Some("○ 신분증\n○ 통장사본")))
        .await;
    assert_eq!(fx.row().await, filled);

    // A run whose detail read failed keeps the stored text, unchanged or not.
    let (outcome, _) = fx.fetch(list.clone()).await;
    assert_eq!(outcome, ProcessOutcome::Unchanged);
    assert_eq!(fx.row().await, filled);
    let edited = list_row(&fx.source_id, "청년 월세 지원 (2차)");
    let (outcome, status) = fx.fetch(edited.clone()).await;
    assert_eq!(
        (outcome, status.as_str()),
        (ProcessOutcome::Changed, "upserted")
    );
    let row = fx.row().await;
    assert_eq!(row.title, "청년 월세 지원 (2차)");
    assert_eq!(row.documents.as_deref(), Some("○ 신분증\n○ 통장사본"));

    // A changed record with its detail row takes the new text; a detail row
    // without the text (an upstream gap) keeps it, changed record or not.
    let renamed = list_row(&fx.source_id, "청년 월세 지원 (3차)");
    fx.fetch(with_detail(renamed.clone(), Some("해당없음")))
        .await;
    assert_eq!(fx.row().await.documents.as_deref(), Some("해당없음"));
    let (outcome, _) = fx.fetch(with_detail(renamed, None)).await;
    assert_eq!(outcome, ProcessOutcome::Unchanged);
    assert_eq!(fx.row().await.documents.as_deref(), Some("해당없음"));
    let (outcome, _) = fx
        .fetch(with_detail(
            list_row(&fx.source_id, "청년 월세 지원 (4차)"),
            None,
        ))
        .await;
    assert_eq!(outcome, ProcessOutcome::Changed);
    assert_eq!(fx.row().await.documents.as_deref(), Some("해당없음"));
    fx.cleanup().await;
}
