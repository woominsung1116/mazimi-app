//! View counters (`조회수`, `inqCnt`, `inqNum`) change on every read of a
//! public catalogue. They must not mark a record changed, re-normalize its
//! programme or re-enqueue it for AI extraction.
use super::*;
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

struct Fixture {
    pool: PgPool,
    source_id: String,
    runs: Vec<Uuid>,
}

impl Fixture {
    async fn new() -> Self {
        Self {
            pool: test_pool().await,
            source_id: format!("volatile-{}", Uuid::new_v4().simple()),
            runs: Vec::new(),
        }
    }

    fn payload(&self, title: &str, views: i64) -> Value {
        json!({
            "서비스ID": self.source_id, "서비스명": title, "신청기한": "상시신청",
            "수정일시": "20260807162419", "조회수": views,
        })
    }

    /// One fresh fetch of the record, hashed the way the source always did
    /// (the whole payload), so the pipeline must not rely on the record's
    /// own hash. Returns the ingestion_items status.
    async fn fetch(&mut self, payload: Value) -> String {
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'gov_benefits','running',now(),'ingestion')")
            .bind(run).execute(&self.pool).await.unwrap();
        self.runs.push(run);
        let record = RawRecord {
            source_id: self.source_id.clone(),
            content_hash: crate::sources::content_hash(&payload),
            payload,
        };
        process_record(
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
        sqlx::query_scalar("SELECT status FROM ingestion_items WHERE run_id=$1 AND source_id=$2")
            .bind(run)
            .bind(&self.source_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn program(&self) -> (Uuid, Value) {
        sqlx::query_as("SELECT id, raw_payload FROM programs WHERE source_type='gov_benefits' AND source_id=$1")
            .bind(&self.source_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// (content_hash, normalizer_version, views in the retained payload)
    async fn snapshot(&self) -> (String, i32, i64) {
        sqlx::query_as("SELECT content_hash, normalizer_version, (raw_payload->>'조회수')::bigint FROM source_snapshots WHERE source_name='gov_benefits' AND source_id=$1")
            .bind(&self.source_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn queued(&self) -> Option<String> {
        let (id, _) = self.program().await;
        sqlx::query_scalar("SELECT reason FROM program_ai_extraction_queue WHERE program_id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
    }

    async fn clear_queue(&self) {
        let (id, _) = self.program().await;
        sqlx::query("DELETE FROM program_ai_extraction_queue WHERE program_id=$1")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
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
async fn a_view_count_change_is_not_a_content_change() {
    let mut fx = Fixture::new().await;
    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원", 100)).await,
        "upserted"
    );
    let (program_id, stored) = fx.program().await;
    fx.clear_queue().await;

    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원", 101)).await,
        "unchanged"
    );
    // Same source_id, same programme row: users' bookmarks, applications
    // and alerts keep pointing at it.
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, source_id FROM programs WHERE source_type='gov_benefits' AND source_id=$1",
    )
    .bind(&fx.source_id)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(program_id, fx.source_id.clone())]);
    // The programme keeps its row; only the snapshot carries the new count.
    assert_eq!(fx.program().await.1, stored);
    assert_eq!(fx.snapshot().await.2, 101);
    assert_eq!(fx.queued().await, None);

    // A real edit still goes through and re-enqueues the programme.
    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원 2차", 102)).await,
        "upserted"
    );
    assert_eq!(fx.queued().await.as_deref(), Some("source_changed"));
    fx.cleanup().await;
}

/// The 보조금24 `detail` row (serviceDetail) is side data for the link
/// resolver: adding it, changing it or losing it on a failed read is not a
/// content change, so it re-normalizes and re-enqueues nothing, and the
/// snapshot always carries the latest read for the resolver.
#[tokio::test]
async fn a_gov24_detail_row_is_not_a_content_change() {
    let mut fx = Fixture::new().await;
    let plain = fx.payload("청년 월세 지원", 100);
    assert_eq!(fx.fetch(plain.clone()).await, "upserted");
    let (program_id, stored) = fx.program().await;
    fx.clear_queue().await;
    let mut with_detail = plain.clone();
    with_detail["detail"] =
        json!({"온라인신청사이트URL": "https://www.bokjiro.go.kr/apply", "접수기관명": "주민센터"});
    assert_eq!(fx.fetch(with_detail.clone()).await, "unchanged");
    let mut changed = with_detail.clone();
    changed["detail"]["온라인신청사이트URL"] = json!("https://www.gov.kr/other");
    assert_eq!(fx.fetch(changed).await, "unchanged");
    assert_eq!(fx.fetch(plain).await, "unchanged");
    assert_eq!(fx.fetch(with_detail.clone()).await, "unchanged");
    // The row keeps its identity and stable payload (no renormalization, no
    // AI enqueue, pins valid); only its side detail follows the latest read,
    // and a read without a detail (plain) kept the stored one.
    let (id_after, row_after) = fx.program().await;
    assert_eq!(id_after, program_id);
    assert_eq!(
        row_after["detail"], with_detail["detail"],
        "the side detail is backfilled"
    );
    let stable = |v: &Value| {
        let mut v = v.clone();
        v.as_object_mut().unwrap().remove("detail");
        v.as_object_mut().unwrap().remove("조회수");
        v
    };
    assert_eq!(stable(&row_after), stable(&stored));
    let snapshot: Value = sqlx::query_scalar("SELECT raw_payload->'detail' FROM source_snapshots WHERE source_name='gov_benefits' AND source_id=$1")
        .bind(&fx.source_id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(snapshot, with_detail["detail"]);
    assert_eq!(fx.queued().await, None);
    fx.cleanup().await;
}

/// The first run after the deploy meets snapshots hashed the old way (the
/// whole payload). Content that only differs by a counter is a rehash, not
/// a change: no programme upsert, the applied normalizer version is kept.
#[tokio::test]
async fn a_snapshot_hashed_with_its_counters_is_rehashed_not_changed() {
    let mut fx = Fixture::new().await;
    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원", 100)).await,
        "upserted"
    );
    let old = fx.payload("청년 월세 지원", 100);
    sqlx::query("UPDATE source_snapshots SET content_hash=$2 WHERE source_name='gov_benefits' AND source_id=$1")
        .bind(&fx.source_id)
        .bind(crate::sources::content_hash(&old))
        .execute(&fx.pool)
        .await
        .unwrap();
    let (_, version_before, _) = fx.snapshot().await;
    assert_eq!(version_before, normalizer_version("gov_benefits"));
    let (_, stored) = fx.program().await;
    fx.clear_queue().await;

    let new = fx.payload("청년 월세 지원", 180);
    assert_eq!(fx.fetch(new.clone()).await, "unchanged");
    let (hash, version, views) = fx.snapshot().await;
    assert_eq!(hash, crate::sources::record_hash("gov_benefits", &new));
    assert_eq!((version, views), (version_before, 180));
    assert_eq!(fx.program().await.1, stored);
    assert_eq!(fx.queued().await, None);
    fx.cleanup().await;
}

/// The queue trigger ignores the same keys as the change hash, so a
/// renormalization that rewrites a counter enqueues nothing.
#[tokio::test]
async fn the_ai_queue_trigger_ignores_view_counters() {
    let mut fx = Fixture::new().await;
    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원", 100)).await,
        "upserted"
    );
    let (id, _) = fx.program().await;
    fx.clear_queue().await;
    sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"조회수\": 999}' WHERE id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(fx.queued().await, None);
    sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"지원내용\": \"월 20만원\"}' WHERE id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(fx.queued().await.as_deref(), Some("source_changed"));
    fx.cleanup().await;
}

/// `ingestion_stable_payload` (SQL) and `sources::volatile_keys` (Rust)
/// are one list kept in two places; this fails when they drift.
#[tokio::test]
async fn the_sql_stable_payload_matches_the_rust_volatile_keys() {
    let pool = test_pool().await;
    let candidates = [
        "조회수",
        "inqCnt",
        "inqNum",
        "sprtSclCnt",
        "수정일시",
        "lastModYmd",
        "detail",
    ];
    for source in [
        "gov_benefits",
        "youth_center",
        "local_welfare",
        "national_welfare",
        "scholarship",
        "fss_financial",
    ] {
        let mut payload = json!({"title": "x"});
        for key in candidates {
            payload[key] = json!("1");
        }
        let stripped: Value = sqlx::query_scalar("SELECT ingestion_stable_payload($1, $2)")
            .bind(source)
            .bind(&payload)
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut expected = payload.clone();
        for key in crate::sources::volatile_keys(source) {
            expected.as_object_mut().unwrap().remove(*key);
        }
        assert_eq!(stripped, expected, "{source}");
    }
    let scalar: Value =
        sqlx::query_scalar("SELECT ingestion_stable_payload('gov_benefits', '[1]'::jsonb)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(scalar, json!([1]));
    pool.close().await;
}

/// An approved AI value is pinned to the programme's stable payload (view
/// counters left out), so a re-normalization whose payload only differs by
/// a counter keeps the pin; a pin of the full legacy payload still applies
/// to that same payload; a real edit drops both.
#[tokio::test]
async fn a_pin_survives_a_view_counter_change_but_not_an_edit() {
    let mut fx = Fixture::new().await;
    assert_eq!(
        fx.fetch(fx.payload("청년 월세 지원", 100)).await,
        "upserted"
    );
    let (id, _) = fx.program().await;
    let cache: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_cache(program_id,source_type,source_hash,provider,model,prompt_version,schema_version,request_hash,source_text,status,response_json)
        VALUES($1,'test',repeat('a',64),'mock','m','p',1,repeat('b',64),'x','succeeded','{}') RETURNING id")
        .bind(id).fetch_one(&fx.pool).await.unwrap();
    let candidate: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_candidates(cache_id,program_id,source_hash,provider,model,prompt_version,schema_version,fact_key,fact_type,value,evidence,confidence,status,reviewed_at,review_note)
        VALUES($1,$2,repeat('a',64),'mock','m','p',1,'age','age','{}','{}','HIGH','approved',now(),'test approval note') RETURNING id")
        .bind(cache).bind(id).fetch_one(&fx.pool).await.unwrap();
    // min_age: the key the writer stores now; max_age: a legacy key.
    sqlx::query("INSERT INTO program_ai_field_overrides(program_id,field,int_value,raw_payload_md5,candidate_id)
        SELECT id,'min_age',27,md5(ingestion_stable_payload(source_type,raw_payload)::text),$2 FROM programs WHERE id=$1
        UNION ALL SELECT id,'max_age',33,md5(raw_payload::text),$2 FROM programs WHERE id=$1")
        .bind(id).bind(candidate).execute(&fx.pool).await.unwrap();
    let ages = |pool: PgPool| async move {
        sqlx::query_as::<_, (Option<i32>, Option<i32>)>(
            "SELECT min_age, max_age FROM programs WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let renormalize = |pool: PgPool, source_id: String| async move {
        sqlx::query("UPDATE source_snapshots SET normalizer_version=0 WHERE source_name='gov_benefits' AND source_id=$1")
            .bind(source_id).execute(&pool).await.unwrap();
    };

    // Same content, same counter: both pins apply.
    renormalize(fx.pool.clone(), fx.source_id.clone()).await;
    fx.fetch(fx.payload("청년 월세 지원", 100)).await;
    assert_eq!(ages(fx.pool.clone()).await, (Some(27), Some(33)));

    // Only the counter moved, and the row is re-normalized with it: the
    // stable pin holds. (The legacy pin hashed the counter and lapses.)
    renormalize(fx.pool.clone(), fx.source_id.clone()).await;
    fx.fetch(fx.payload("청년 월세 지원", 140)).await;
    assert_eq!(fx.program().await.1["조회수"], 140);
    assert_eq!(ages(fx.pool.clone()).await.0, Some(27));

    // A real edit: the pin no longer applies.
    fx.fetch(fx.payload("청년 월세 지원 2차", 141)).await;
    assert_eq!(ages(fx.pool.clone()).await.0, None);
    sqlx::query("DELETE FROM program_ai_field_overrides WHERE program_id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM program_ai_extraction_candidates WHERE program_id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM program_ai_extraction_cache WHERE program_id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.cleanup().await;
}

/// `ingestion_stable_payload` is internal to the ingestion trigger and the
/// worker: PUBLIC (so `anon`/`authenticated` on a hosted database) cannot
/// call it, its owner can.
#[tokio::test]
async fn the_stable_payload_function_is_not_executable_by_public() {
    let pool = test_pool().await;
    let (public, owner): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) a
                        WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'),
                has_function_privilege(p.proowner, p.oid, 'EXECUTE')
         FROM pg_proc p WHERE p.oid = 'ingestion_stable_payload(text, jsonb)'::regprocedure",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((public, owner), (false, true));
    pool.close().await;
}

/// 복지로 중앙's detail (`applmetList`) is side data: an unchanged record
/// carrying it fills `raw_payload.detail` (the backfill) without
/// re-normalizing, and a later read without it keeps the stored one.
#[tokio::test]
async fn national_welfare_detail_backfills_an_unchanged_row() {
    let pool = test_pool().await;
    let source_id = format!(
        "national_welfare_WLF{}",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let base = json!({
        "servId": source_id, "servNm": "청년 상세 백필 확인", "servDgst": "청년에게 지원합니다",
        "jurMnofNm": "보건복지부", "jurOrgNm": "청년정책과", "lifeArray": "청년", "srvPvsnNm": "현금지급",
        "sprtCycNm": "1회성", "servDtlLink": "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00000001",
        "inqNum": 1, "source": "national_welfare",
    });
    let detail = json!({"applmetList": [{"stage": "신청기관연락처목록", "text": "거주지 읍/면/동 주민센터에서 ‘서비스 신청’"}]});
    let run = |payload: Value| {
        let pool = pool.clone();
        let source_id = source_id.clone();
        async move {
            let run = Uuid::new_v4();
            sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'national_welfare','running',now(),'ingestion')")
                .bind(run).execute(&pool).await.unwrap();
            let record = RawRecord {
                source_id: source_id.clone(),
                content_hash: crate::sources::content_hash(&payload),
                payload,
            };
            process_record(
                &pool,
                "national_welfare",
                run,
                &record,
                &DedupCaches::default(),
                &HashSet::new(),
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
            let status: String = sqlx::query_scalar(
                "SELECT status FROM ingestion_items WHERE run_id=$1 AND source_id=$2",
            )
            .bind(run)
            .bind(&source_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            let (stored, updated): (Value, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
                "SELECT raw_payload, updated_at FROM programs WHERE source_type='national_welfare' AND source_id=$1",
            )
            .bind(&source_id).fetch_one(&pool).await.unwrap();
            (status, stored, updated)
        }
    };
    let (_, first, _) = run(base.clone()).await;
    assert!(first.get("detail").is_none());
    let mut with_detail = base.clone();
    with_detail["detail"] = detail.clone();
    let (status, stored, _) = run(with_detail).await;
    assert_eq!(status, "unchanged", "the detail never changes the record");
    assert_eq!(stored["detail"], detail, "an unchanged row is backfilled");
    let (status, kept, _) = run(base.clone()).await;
    assert_eq!(status, "unchanged");
    assert_eq!(
        kept["detail"], detail,
        "a read without a detail keeps the stored one"
    );
    // A changed record whose detail read stopped this run keeps the stored one too.
    let mut edited = base.clone();
    edited["servDgst"] = json!("청년에게 지원합니다 (수정)");
    let (status, changed, _) = run(edited).await;
    assert_ne!(status, "unchanged");
    assert_eq!(changed["servDgst"], json!("청년에게 지원합니다 (수정)"));
    assert_eq!(
        changed["detail"], detail,
        "a changed row keeps its stored detail"
    );
    let stable_equal: bool = sqlx::query_scalar("SELECT ingestion_stable_payload('national_welfare', $1) = ingestion_stable_payload('national_welfare', $2)")
        .bind(&stored).bind(&first).fetch_one(&pool).await.unwrap();
    assert!(stable_equal, "AI pins (stable payload) ignore the detail");
    sqlx::query("DELETE FROM programs WHERE source_type='national_welfare' AND source_id=$1")
        .bind(&source_id)
        .execute(&pool)
        .await
        .ok();
}
