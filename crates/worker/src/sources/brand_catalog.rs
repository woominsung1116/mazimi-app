//! 공정거래위원회 가맹 브랜드 카탈로그 수집기.
//!
//! This source creates the searchable brand universe. It does not fabricate or
//! infer benefits; benefits are linked later through `program_brands` after an
//! official/provider page has been verified.
//!
//! API: https://apis.data.go.kr/1130000/FftcBrandRlsInfo2_Service/getBrandinfo
//! Env: FTC_BRAND_API_KEY (falls back to GOV_API_KEY)
//!      FTC_BRAND_BASE_YEAR (defaults to previous calendar year)

use anyhow::{bail, Context, Result};
use chrono::{Datelike, Utc};
use mazimi_core::normalize_brand_name;
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    error_safety::persisted_error,
    operations::{self, EventCommand, Severity},
};

use super::{content_hash, http, DataSource, RawRecord};

const BASE_URL: &str = "https://apis.data.go.kr/1130000/FftcBrandRlsInfo2_Service/getBrandinfo";
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 500;

pub struct FtcBrandCatalogSource {
    client: Client,
    api_key: String,
    base_year: i32,
}

impl FtcBrandCatalogSource {
    pub fn new(api_key: String, base_year: i32) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("MazimiBrandCatalog/1.0")
                .build()
                .unwrap_or_default(),
            api_key,
            base_year,
        }
    }

    pub fn from_env() -> Result<Self> {
        let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "production".to_string());
        let production_approved = std::env::var("FTC_BRAND_PRODUCTION_APPROVED").ok();
        if !ftc_use_is_approved(&app_env, production_approved.as_deref()) {
            bail!(
                "FTC brand API production use is disabled until FTC_BRAND_PRODUCTION_APPROVED=true"
            );
        }
        let api_key = std::env::var("FTC_BRAND_API_KEY")
            .or_else(|_| std::env::var("GOV_API_KEY"))
            .context("FTC_BRAND_API_KEY (or GOV_API_KEY) not set")?;
        if api_key.trim().is_empty() {
            bail!("FTC_BRAND_API_KEY (or GOV_API_KEY) is empty");
        }

        let default_year = Utc::now().year() - 1;
        let base_year = std::env::var("FTC_BRAND_BASE_YEAR")
            .ok()
            .map(|value| value.parse::<i32>())
            .transpose()
            .context("FTC_BRAND_BASE_YEAR must be a four-digit year")?
            .unwrap_or(default_year);
        if !(2000..=2100).contains(&base_year) {
            bail!("FTC_BRAND_BASE_YEAR must be between 2000 and 2100");
        }

        Ok(Self::new(api_key, base_year))
    }

    pub fn base_year(&self) -> i32 {
        self.base_year
    }

    async fn fetch_page(&self, page: usize) -> Result<(Vec<Value>, Option<usize>)> {
        let page = page.to_string();
        let rows = PAGE_SIZE.to_string();
        let year = self.base_year.to_string();
        let response = http::send(
            self.client.get(BASE_URL).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("pageNo", page.as_str()),
                ("numOfRows", rows.as_str()),
                ("resultType", "json"),
                ("jngBizCrtraYr", year.as_str()),
            ]),
            "ftc_brand_catalog",
        )
        .await?;
        let text = http::text(response, "ftc_brand_catalog").await?;

        let payload: Value =
            serde_json::from_str(&text).context("ftc_brand_catalog: JSON schema changed")?;
        ensure_api_success(&payload)?;
        let items = extract_items(&payload)?;
        let total = extract_total_count(&payload);
        Ok((items, total))
    }
}

fn ftc_use_is_approved(app_env: &str, approval_flag: Option<&str>) -> bool {
    app_env == "local"
        || approval_flag.is_some_and(|value| {
            matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        })
}

impl DataSource for FtcBrandCatalogSource {
    fn name(&self) -> &'static str {
        "ftc_brand_catalog"
    }

    async fn fetch_all(&self) -> Result<super::SourceFetch> {
        let mut records = Vec::new();
        let mut seen_source_ids = HashSet::new();

        for page in 1..=MAX_PAGES {
            info!(
                source = self.name(),
                page,
                base_year = self.base_year,
                "fetching brand page"
            );
            let (items, total_count) = self.fetch_page(page).await?;
            if items.is_empty() {
                break;
            }

            let fetched = items.len();
            let mut new_on_page = 0usize;
            for item in items {
                let hash = content_hash(&item);
                let source_id = source_id_for_item(&item, &hash);
                if seen_source_ids.insert(source_id.clone()) {
                    records.push(RawRecord {
                        source_id,
                        payload: item,
                        content_hash: hash,
                    });
                    new_on_page += 1;
                }
            }

            if new_on_page == 0 {
                warn!(
                    source = self.name(),
                    page, "upstream repeated a page; stopping pagination"
                );
                break;
            }
            if total_count.is_some_and(|total| records.len() >= total) || fetched < PAGE_SIZE {
                break;
            }
        }

        if records.is_empty() {
            bail!(
                "ftc_brand_catalog returned no brands for base year {}; set FTC_BRAND_BASE_YEAR to an available year",
                self.base_year
            );
        }
        Ok(super::SourceFetch::complete(records))
    }
}

#[derive(Debug)]
struct NormalizedBrand {
    name: String,
    normalized_name: String,
    company_name: Option<String>,
    industry_major: Option<String>,
    industry_minor: Option<String>,
    primary_product: Option<String>,
}

fn first_string<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalize_record(payload: &Value) -> Result<NormalizedBrand> {
    let name = first_string(payload, &["brandNm", "브랜드명"])
        .context("brand record is missing brandNm")?
        .to_string();
    let normalized_name = normalize_brand_name(&name);
    if normalized_name.is_empty() {
        bail!("brand record has no searchable characters");
    }

    Ok(NormalizedBrand {
        name,
        normalized_name,
        company_name: first_string(
            payload,
            &[
                "corpNm",
                "jnghdqrtrsFmnm",
                "jnghdqrtrsNm",
                "법인명",
                "가맹본부상호명",
            ],
        )
        .map(str::to_string),
        industry_major: first_string(payload, &["indutyLclasNm", "업종대분류명"])
            .map(str::to_string),
        industry_minor: first_string(payload, &["indutyMlsfcNm", "업종중분류명"])
            .map(str::to_string),
        primary_product: first_string(
            payload,
            &["mnfrnNm", "mainPrdlstNm", "mrhstGoodsNm", "주요상품명"],
        )
        .map(str::to_string),
    })
}

fn source_id_for_item(payload: &Value, fallback_hash: &str) -> String {
    first_string(payload, &["brandMnno", "브랜드관리번호"])
        .map(str::to_string)
        .or_else(|| {
            let name = first_string(payload, &["brandNm", "브랜드명"])?;
            let company =
                first_string(payload, &["corpNm", "jnghdqrtrsFmnm", "법인명"]).unwrap_or("");
            let key = normalize_brand_name(&format!("{company}{name}"));
            (!key.is_empty()).then_some(format!("name:{key}"))
        })
        .unwrap_or_else(|| format!("hash:{fallback_hash}"))
}

fn ensure_api_success(payload: &Value) -> Result<()> {
    let code = payload
        .pointer("/response/header/resultCode")
        .or_else(|| payload.pointer("/header/resultCode"))
        .or_else(|| payload.get("resultCode"))
        .and_then(Value::as_str);
    if code.is_some_and(|code| !matches!(code, "0" | "00" | "0000")) {
        let message = payload
            .pointer("/response/header/resultMsg")
            .or_else(|| payload.pointer("/header/resultMsg"))
            .or_else(|| payload.get("resultMsg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown API error");
        bail!("ftc_brand_catalog API error {code:?}: {message}");
    }
    Ok(())
}

fn extract_items(payload: &Value) -> Result<Vec<Value>> {
    let candidates = [
        "/response/body/items/item",
        "/response/body/items",
        "/body/items/item",
        "/body/items",
        "/items/item",
        "/items",
    ];

    for path in candidates {
        if let Some(value) = payload.pointer(path) {
            return match value {
                Value::Array(items) => Ok(items.clone()),
                Value::Object(_) => Ok(vec![value.clone()]),
                Value::Null => Ok(Vec::new()),
                _ => bail!("ftc_brand_catalog: unexpected items shape at {path}"),
            };
        }
    }
    bail!("ftc_brand_catalog: response has no items field")
}

fn extract_total_count(payload: &Value) -> Option<usize> {
    [
        "/response/body/totalCount",
        "/body/totalCount",
        "/totalCount",
    ]
    .iter()
    .find_map(|path| payload.pointer(path))
    .and_then(|value| {
        value
            .as_u64()
            .map(|n| n as usize)
            .or_else(|| value.as_str()?.parse().ok())
    })
}

/// Fetch and upsert the full FTC catalogue while preserving raw provenance.
pub async fn run_brand_catalog_ingestion(
    pool: &PgPool,
    source: &FtcBrandCatalogSource,
) -> Result<()> {
    let run_id = Uuid::new_v4();
    let started_at = Utc::now();
    sqlx::query(
        "INSERT INTO ingestion_runs (id, source_name, status, started_at)
         VALUES ($1, $2, 'running', $3)",
    )
    .bind(run_id)
    .bind(source.name())
    .bind(started_at)
    .execute(pool)
    .await?;
    crate::daily::note_job_run(run_id);

    let result = process_catalog(pool, source, run_id).await;
    match &result {
        Ok((total, changed, failed)) => {
            sqlx::query(
                "UPDATE ingestion_runs
                 SET status = 'success', finished_at = now(), total_fetched = $2,
                     total_changed = $3, total_failed = $4
                 WHERE id = $1",
            )
            .bind(run_id)
            .bind(*total as i32)
            .bind(*changed as i32)
            .bind(*failed as i32)
            .execute(pool)
            .await?;
            info!(
                source = source.name(),
                run_id = %run_id,
                total,
                changed,
                "brand catalogue ingestion complete"
            );
            let hard_fingerprint = "ingestion_failed:ftc_brand_catalog";
            let partial_fingerprint = "ingestion_partial:ftc_brand_catalog";
            if let Err(event_error) = operations::resolve_event(
                pool,
                hard_fingerprint,
                None,
                Some("brand catalogue ingestion recovered"),
            )
            .await
            {
                warn!(error = %event_error, "failed to resolve brand catalogue incident");
            }
            if *failed > 0 {
                let run_id_text = run_id.to_string();
                if let Err(event_error) = operations::record_event(
                    pool,
                    &EventCommand {
                        fingerprint: partial_fingerprint,
                        severity: Severity::Warning,
                        component: "ingestion",
                        event_type: "brand_catalog_partial_failure",
                        subject_type: Some("ingestion_run"),
                        subject_id: Some(&run_id_text),
                        message: "Brand catalogue ingestion completed with rejected records",
                        details: serde_json::json!({ "total": total, "failed": failed }),
                    },
                )
                .await
                {
                    warn!(error = %event_error, "failed to record brand catalogue incident");
                }
            } else if let Err(event_error) = operations::resolve_event(
                pool,
                partial_fingerprint,
                None,
                Some("brand catalogue completed without rejected records"),
            )
            .await
            {
                warn!(error = %event_error, "failed to resolve brand catalogue partial incident");
            }
        }
        Err(err) => {
            sqlx::query(
                "UPDATE ingestion_runs
                 SET status = 'failed', finished_at = now(), error_message = $2
                 WHERE id = $1",
            )
            .bind(run_id)
            .bind(persisted_error(err))
            .execute(pool)
            .await?;
            error!(source = source.name(), run_id = %run_id, error = %err, "brand catalogue ingestion failed");
            let run_id_text = run_id.to_string();
            if let Err(event_error) = operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: "ingestion_failed:ftc_brand_catalog",
                    severity: Severity::Error,
                    component: "ingestion",
                    event_type: "brand_catalog_ingestion_failed",
                    subject_type: Some("ingestion_run"),
                    subject_id: Some(&run_id_text),
                    message: "Brand catalogue ingestion failed",
                    details: serde_json::json!({ "base_year": source.base_year() }),
                },
            )
            .await
            {
                warn!(error = %event_error, "failed to record brand catalogue failure");
            }
        }
    }
    result.map(|_| ())
}

async fn process_catalog(
    pool: &PgPool,
    source: &FtcBrandCatalogSource,
    run_id: Uuid,
) -> Result<(usize, usize, usize)> {
    let records = source.fetch_all().await?.records;
    let total = records.len();
    let mut changed = 0usize;
    let mut failed = 0usize;

    for record in &records {
        match process_brand_record(pool, source.name(), run_id, record).await {
            Ok(was_changed) => changed += usize::from(was_changed),
            Err(err) => {
                failed += 1;
                warn!(source_id = %record.source_id, error = %err, "brand record failed");
                preserve_failed_record(pool, source.name(), run_id, record, &err).await;
            }
        }
    }
    if failed == total {
        bail!("all {total} brand records failed normalization or persistence");
    }
    if failed > 0 {
        warn!(
            source = source.name(),
            total, changed, failed, "brand catalogue completed with failed records"
        );
    }
    Ok((total, changed, failed))
}

async fn process_brand_record(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    record: &RawRecord,
) -> Result<bool> {
    let previous: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT content_hash, brand_id
         FROM brand_source_records
         WHERE source_type = $1 AND source_id = $2",
    )
    .bind(source_name)
    .bind(&record.source_id)
    .fetch_optional(pool)
    .await?;

    if previous
        .as_ref()
        .is_some_and(|(hash, brand_id)| hash == &record.content_hash && brand_id.is_some())
    {
        let brand_id = previous.and_then(|(_, brand_id)| brand_id);
        let mut tx = pool.begin().await?;
        sqlx::query(
            "UPDATE brand_source_records SET fetched_at = now()
             WHERE source_type = $1 AND source_id = $2",
        )
        .bind(source_name)
        .bind(&record.source_id)
        .execute(&mut *tx)
        .await?;
        if let Some(brand_id) = brand_id {
            sqlx::query("UPDATE brands SET last_synced_at = now() WHERE id = $1")
                .bind(brand_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "INSERT INTO ingestion_items
                 (id, run_id, source_id, brand_id, status, created_at)
             VALUES ($1, $2, $3, $4, 'unchanged', now())
             ON CONFLICT (run_id, source_id) DO UPDATE
                 SET brand_id = EXCLUDED.brand_id, status = 'unchanged', error_message = NULL",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(&record.source_id)
        .bind(brand_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(false);
    }

    let brand = normalize_record(&record.payload)?;
    let mut tx = pool.begin().await?;
    let brand_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO brands (
             id, name, normalized_name, company_name, industry_major,
             industry_minor, primary_product, source_type, source_id,
             discovery_priority, is_active, last_synced_at, created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, $5, $6, $7, $8, $9, 10, true, now(), now(), now()
         )
         ON CONFLICT (source_type, source_id) DO UPDATE SET
             name = EXCLUDED.name,
             normalized_name = EXCLUDED.normalized_name,
             company_name = COALESCE(EXCLUDED.company_name, brands.company_name),
             industry_major = EXCLUDED.industry_major,
             industry_minor = EXCLUDED.industry_minor,
             primary_product = EXCLUDED.primary_product,
             is_active = true,
             last_synced_at = now(),
             updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&brand.name)
    .bind(&brand.normalized_name)
    .bind(&brand.company_name)
    .bind(&brand.industry_major)
    .bind(&brand.industry_minor)
    .bind(&brand.primary_product)
    .bind(source_name)
    .bind(&record.source_id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO brand_source_records
             (id, brand_id, source_type, source_id, content_hash, raw_payload, fetched_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (source_type, source_id) DO UPDATE SET
             brand_id = EXCLUDED.brand_id,
             content_hash = EXCLUDED.content_hash,
             raw_payload = EXCLUDED.raw_payload,
             fetched_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(brand_id)
    .bind(source_name)
    .bind(&record.source_id)
    .bind(&record.content_hash)
    .bind(&record.payload)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO ingestion_items
             (id, run_id, source_id, brand_id, status, created_at)
         VALUES ($1, $2, $3, $4, 'brand_upserted', now())
         ON CONFLICT (run_id, source_id) DO UPDATE
             SET brand_id = EXCLUDED.brand_id, status = 'brand_upserted', error_message = NULL",
    )
    .bind(Uuid::new_v4())
    .bind(run_id)
    .bind(&record.source_id)
    .bind(brand_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

async fn preserve_failed_record(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    record: &RawRecord,
    error: &anyhow::Error,
) {
    let _ = sqlx::query(
        "INSERT INTO brand_source_records
             (id, source_type, source_id, content_hash, raw_payload, fetched_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (source_type, source_id) DO UPDATE SET
             content_hash = EXCLUDED.content_hash,
             raw_payload = EXCLUDED.raw_payload,
             fetched_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(source_name)
    .bind(&record.source_id)
    .bind(&record.content_hash)
    .bind(&record.payload)
    .execute(pool)
    .await;

    let _ = sqlx::query(
        "INSERT INTO ingestion_items
             (id, run_id, source_id, status, error_message, created_at)
         VALUES ($1, $2, $3, 'failed', $4, now())
         ON CONFLICT (run_id, source_id) DO UPDATE
             SET status = 'failed', error_message = EXCLUDED.error_message",
    )
    .bind(Uuid::new_v4())
    .bind(run_id)
    .bind(&record.source_id)
    .bind(persisted_error(error))
    .execute(pool)
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn production_requires_explicit_provider_approval() {
        assert!(ftc_use_is_approved("local", None));
        assert!(!ftc_use_is_approved("production", None));
        assert!(!ftc_use_is_approved("staging", Some("false")));
        assert!(ftc_use_is_approved("production", Some("true")));
    }

    #[test]
    fn extracts_wrapped_item_array() {
        let payload = json!({
            "response": {
                "header": { "resultCode": "00", "resultMsg": "NORMAL SERVICE" },
                "body": {
                    "items": { "item": [{"brandNm": "테스트"}] },
                    "totalCount": 1
                }
            }
        });
        let items = extract_items(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(extract_total_count(&payload), Some(1));
    }

    #[test]
    fn normalizes_ftc_brand_fields() {
        let payload = json!({
            "brandNm": "A-B 카페",
            "corpNm": "에이비 주식회사",
            "indutyLclasNm": "외식",
            "indutyMlsfcNm": "커피",
            "mnfrnNm": "커피, 디저트"
        });
        let brand = normalize_record(&payload).unwrap();
        assert_eq!(brand.name, "A-B 카페");
        assert_eq!(brand.normalized_name, "ab카페");
        assert_eq!(brand.company_name.as_deref(), Some("에이비 주식회사"));
        assert_eq!(brand.industry_minor.as_deref(), Some("커피"));
    }

    #[test]
    fn prefers_stable_brand_management_number() {
        let payload = json!({"brandMnno": "BRD_123", "brandNm": "테스트"});
        assert_eq!(source_id_for_item(&payload, "fallback"), "BRD_123");
    }

    #[test]
    fn rejects_upstream_error_code() {
        let payload = json!({
            "response": {"header": {"resultCode": "30", "resultMsg": "KEY ERROR"}}
        });
        assert!(ensure_api_success(&payload).is_err());
    }
}
