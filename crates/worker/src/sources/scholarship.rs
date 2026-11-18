//! 한국장학재단(KOSAF) 장학금 Open API client
//!
//! API: https://api.odcloud.kr/api/15028252/v1/uddi:d0fb69e9-e143-412d-9fe0-b0d87a16f3ff
//! Env: KOSAF_API_KEY, SCHOLARSHIP_DATASET_AUTO (default true)
//!
//! The provider publishes the catalogue as dated files under one dataset id
//! (`한국장학재단_학자금지원정보(대학생)_YYYYMMDD`), each with its own UDDI.
//! `BASE_URL` pins the 2025-02-20 file, which by 2026-10 held only closed
//! scholarships. Each run therefore reads the dataset's public OpenAPI
//! listing (`DATASET_DOCS_URL`, no key) and tries, in order:
//! 1. the newest files (up to three) dated after the last file actually used
//!    whose schema still has the fields the normalizer reads, each used only
//!    when it is non-empty and has at least one future deadline;
//! 2. the last file actually used (recorded by the worker), non-empty;
//! 3. the pinned file, only when no newer file has ever been used.
//!
//! When a newer file has been used and none of 1-2 works, the fetch fails:
//! the run records no upsert and yesterday's catalogue stays as it is.
//! Falling back to the pinned 2025 file then would overwrite live rows (same
//! source ids) with 2025 deadlines and close them.
//!
//! Response format: JSON (flat data array)
//! Pagination: page (1-based), perPage

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

use super::{
    content_hash, effective_page_size, ensure_complete, http, DataSource, RawRecord, SourceFetch,
};

const BASE_URL: &str =
    "https://api.odcloud.kr/api/15028252/v1/uddi:d0fb69e9-e143-412d-9fe0-b0d87a16f3ff";
const API_ROOT: &str = "https://api.odcloud.kr/api";
const PINNED_PATH: &str = "/15028252/v1/uddi:d0fb69e9-e143-412d-9fe0-b0d87a16f3ff";
const PINNED_DATE: &str = "20250220";
const DATASET_DOCS_URL: &str = "https://infuser.odcloud.kr/oas/docs?namespace=15028252/v1";
const DATASET_TITLE: &str = "학자금지원정보(대학생)_";
/// Fields the normalizer and the source id depend on.
const REQUIRED_FIELDS: &[&str] = &["상품명", "운영기관명", "모집시작일", "모집종료일"];
const PAGE_SIZE: u32 = 100;

pub struct ScholarshipSource {
    client: Client,
    api_key: String,
    auto_dataset: bool,
    /// The newest non-pinned file a previous run used: `(path, YYYYMMDD)`.
    last_used: Option<(String, String)>,
    choice: std::sync::Mutex<Option<DatasetChoice>>,
}

const MAX_NEWER_ATTEMPTS: usize = 3;

/// One file to try, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub path: String,
    pub date: String,
    /// Newer files must carry a future deadline; the last used file only has
    /// to be non-empty (re-reading it changes nothing users see).
    pub require_future: bool,
    pub reason: &'static str,
}

fn valid_path(path: &str) -> bool {
    regex::Regex::new(r"^/15028252/v1/uddi:[0-9a-f-]{36}(_[0-9]{12})?$")
        .expect("static regex")
        .is_match(path)
}

/// The attempts in order, and whether the pinned file may be the last
/// resort (only when no newer file has ever been used).
pub fn plan_attempts(
    listed: &[(String, String)],
    last_used: Option<&(String, String)>,
) -> (Vec<Attempt>, bool) {
    let last_used = last_used
        .filter(|(path, date)| valid_path(path) && date.len() == 8 && date.as_str() > PINNED_DATE);
    let floor = last_used.map_or(PINNED_DATE, |(_, date)| date.as_str());
    let mut newer: Vec<&(String, String)> = listed
        .iter()
        .filter(|(path, date)| path != PINNED_PATH && date.as_str() > floor)
        .collect();
    newer.sort_by(|a, b| b.1.cmp(&a.1));
    let mut attempts: Vec<Attempt> = newer
        .into_iter()
        .take(MAX_NEWER_ATTEMPTS)
        .enumerate()
        .map(|(index, (path, date))| Attempt {
            path: path.clone(),
            date: date.clone(),
            require_future: true,
            reason: if index == 0 {
                "newest_valid_dataset"
            } else {
                "next_newest_valid_dataset"
            },
        })
        .collect();
    if let Some((path, date)) = last_used {
        attempts.push(Attempt {
            path: path.clone(),
            date: date.clone(),
            require_future: false,
            reason: "last_used_dataset",
        });
    }
    (attempts, last_used.is_none())
}

/// Runs the plan with `fetch(url)`. Returns the first usable fetch, the
/// pinned file as the permitted last resort, or an error (no upsert).
pub async fn run_plan<F, Fut>(
    attempts: Vec<Attempt>,
    pinned_allowed: bool,
    pinned_reason: &'static str,
    today: chrono::NaiveDate,
    fetch: F,
) -> Result<(SourceFetch, DatasetChoice)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<SourceFetch>>,
{
    let mut failures = Vec::new();
    for attempt in attempts {
        let reason = match fetch(format!("{API_ROOT}{}", attempt.path)).await {
            Err(_) => "fetch_failed",
            Ok(fetched) => {
                let verdict = if attempt.require_future {
                    validate_dataset(&fetched.records, today)
                } else if fetched.records.is_empty() {
                    Err("latest_dataset_empty")
                } else {
                    Ok(())
                };
                match verdict {
                    Ok(()) => {
                        return Ok((
                            fetched,
                            DatasetChoice {
                                path: attempt.path,
                                dataset_date: attempt.date,
                                pinned: false,
                                reason: attempt.reason,
                            },
                        ))
                    }
                    Err(reason) => reason,
                }
            }
        };
        warn!(dataset = %attempt.date, reason, "scholarship dataset not usable");
        failures.push(format!("{}:{reason}", attempt.date));
    }
    if !pinned_allowed {
        anyhow::bail!(
            "scholarship: no usable dataset ({}); keeping the current catalogue",
            failures.join(", ")
        );
    }
    let fetch = fetch(BASE_URL.to_string()).await?;
    Ok((
        fetch,
        DatasetChoice {
            path: PINNED_PATH.into(),
            dataset_date: PINNED_DATE.into(),
            pinned: true,
            reason: pinned_reason,
        },
    ))
}

/// Which dataset file a run used, and why.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct DatasetChoice {
    pub path: String,
    pub dataset_date: String,
    pub pinned: bool,
    pub reason: &'static str,
}

/// The newest listed file whose schema has [`REQUIRED_FIELDS`]:
/// `(path, YYYYMMDD)`. Paths must stay under this dataset id.
pub fn latest_dataset(docs: &Value) -> Option<(String, String)> {
    listed_datasets(docs).into_iter().next()
}

/// Every listed file whose schema has [`REQUIRED_FIELDS`], newest first.
pub fn listed_datasets(docs: &Value) -> Vec<(String, String)> {
    let definitions = docs.get("definitions");
    let Some(paths) = docs.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut listed: Vec<(String, String)> = paths
        .iter()
        .filter(|(path, _)| valid_path(path))
        .filter_map(|(path, operation)| {
            let summary = operation.pointer("/get/summary")?.as_str()?;
            let date = summary.rsplit_once(DATASET_TITLE)?.1;
            if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let model = format!("{}_model", path.trim_start_matches("/15028252/v1/"));
            let properties = definitions?.get(&model)?.get("properties")?;
            REQUIRED_FIELDS
                .iter()
                .all(|field| properties.get(*field).is_some())
                .then(|| (path.clone(), date.to_string()))
        })
        .collect();
    listed.sort_by(|a, b| b.1.cmp(&a.1));
    listed
}

/// A candidate file is used only when it has rows and at least one deadline
/// on or after `today`.
pub fn validate_dataset(
    records: &[RawRecord],
    today: chrono::NaiveDate,
) -> Result<(), &'static str> {
    if records.is_empty() {
        return Err("latest_dataset_empty");
    }
    let has_future = records.iter().any(|record| {
        record
            .payload
            .get("모집종료일")
            .and_then(Value::as_str)
            .and_then(|value| chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok())
            .is_some_and(|end| end >= today)
    });
    if has_future {
        Ok(())
    } else {
        Err("latest_dataset_has_no_future_deadline")
    }
}

impl ScholarshipSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::http::key_in_query_client(None),
            api_key,
            auto_dataset: true,
            last_used: None,
            choice: std::sync::Mutex::new(None),
        }
    }

    /// The newest non-pinned file a previous run used (from the worker's
    /// `scholarship_dataset_switched` records).
    pub fn with_last_used(mut self, last_used: Option<(String, String)>) -> Self {
        self.last_used = last_used;
        self
    }

    /// The dataset file the last `fetch_all` used.
    pub fn dataset_choice(&self) -> Option<DatasetChoice> {
        self.choice.lock().ok().and_then(|choice| choice.clone())
    }

    fn remember(&self, choice: DatasetChoice) {
        if let Ok(mut slot) = self.choice.lock() {
            *slot = Some(choice);
        }
    }

    async fn discover(&self) -> Result<Vec<(String, String)>> {
        let response = http::send(self.client.get(DATASET_DOCS_URL), "scholarship").await?;
        let text = http::text(response, "scholarship").await?;
        let docs: Value =
            serde_json::from_str(&text).context("scholarship: dataset listing is not JSON")?;
        Ok(listed_datasets(&docs))
    }

    async fn fetch_best(&self) -> Result<SourceFetch> {
        let today = chrono::Utc::now()
            .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).expect("offset"))
            .date_naive();
        let fetch = |url: String| async move { self.fetch_from(&url).await };
        if !self.auto_dataset {
            let (fetch, choice) =
                run_plan(Vec::new(), true, "auto_dataset_disabled", today, fetch).await?;
            self.remember(choice);
            return Ok(fetch);
        }
        let (listed, listing_reason) = match self.discover().await {
            Ok(listed) => (listed, "pinned_is_newest"),
            Err(_) => (Vec::new(), "dataset_listing_unavailable"),
        };
        let (attempts, pinned_allowed) = plan_attempts(&listed, self.last_used.as_ref());
        let pinned_reason = if attempts.is_empty() {
            listing_reason
        } else {
            "newer_datasets_not_usable"
        };
        let (fetch, choice) =
            run_plan(attempts, pinned_allowed, pinned_reason, today, fetch).await?;
        if choice.pinned {
            warn!(
                source = self.name(),
                reason = choice.reason,
                "scholarship: using the pinned dataset"
            );
        } else {
            info!(source = self.name(), dataset = %choice.dataset_date, reason = choice.reason, "scholarship dataset chosen");
        }
        self.remember(choice);
        Ok(fetch)
    }

    /// Reads KOSAF_API_KEY, falling back to GOV_API_KEY.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("KOSAF_API_KEY")
            .or_else(|_| std::env::var("GOV_API_KEY"))
            .context("KOSAF_API_KEY (or GOV_API_KEY) not set")?;
        let mut source = Self::new(key);
        source.auto_dataset = !matches!(
            std::env::var("SCHOLARSHIP_DATASET_AUTO")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "false" | "0" | "no"
        );
        Ok(source)
    }

    async fn fetch_page(&self, url: &str, page: u32) -> Result<ScholarshipApiResponse> {
        let response = http::send(
            self.client.get(url).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("page", &page.to_string()),
                ("perPage", &PAGE_SIZE.to_string()),
            ]),
            "scholarship",
        )
        .await?;
        let text = http::text(response, "scholarship").await?;

        serde_json::from_str::<ScholarshipApiResponse>(&text)
            .with_context(|| format!("scholarship: JSON schema changed at page {page}"))
    }
}

impl DataSource for ScholarshipSource {
    fn name(&self) -> &'static str {
        "scholarship"
    }

    async fn fetch_all(&self) -> Result<SourceFetch> {
        self.fetch_best().await
    }
}

impl ScholarshipSource {
    async fn fetch_from(&self, url: &str) -> Result<SourceFetch> {
        let mut records = Vec::new();
        let mut page = 1u32;
        let mut reported_total: Option<u64> = None;

        loop {
            info!(source = self.name(), page, "fetching page");

            let resp = self.fetch_page(url, page).await?;
            let items = resp.data;

            if items.is_empty() {
                warn!(
                    source = self.name(),
                    page, "empty item list — stopping pagination"
                );
                break;
            }

            let fetched = items.len();
            for item in items {
                // Use 상품명 + 운영기관명 combo as a stable source ID since
                // the API does not expose a numeric code.
                let name = item.get("상품명").and_then(|v| v.as_str()).unwrap_or("");
                let org = item
                    .get("운영기관명")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let hash = content_hash(&item);
                // Content-derived, never position-derived: see youth_center.
                let source_id = if name.is_empty() && org.is_empty() {
                    format!("ks_unknown_{}", &hash[..16])
                } else {
                    format!("{}_{}", org, name)
                };

                records.push(RawRecord {
                    source_id,
                    payload: item,
                    content_hash: hash,
                });
            }

            reported_total = resp.matchCount.or(resp.totalCount);
            let page_size = effective_page_size(resp.perPage, fetched, PAGE_SIZE);
            let last_page = (fetched as u64) < page_size
                || reported_total.is_some_and(|total| u64::from(page) * page_size >= total);
            if last_page {
                break;
            }
            anyhow::ensure!(page < 1000, "scholarship: pagination limit exceeded");
            page += 1;
        }

        disambiguate_colliding_ids(&mut records);
        let complete = ensure_complete(self.name(), records.len(), reported_total);
        info!(
            source = self.name(),
            total = records.len(),
            complete,
            "fetch complete"
        );
        Ok(SourceFetch::unfiltered(records, complete))
    }
}

// ── Response shapes ───────────────────────────────────────────────────────────
//
// {
//   "currentCount": 100,
//   "data": [{
//     "상품명": "행복나눔 장학생",
//     "상품구분": "장학금",
//     "운영기관명": "광주남구장학회",
//     "대학구분": "4년제",
//     "학과구분": "제한없음",
//     "학년구분": "대학2학기,...",
//     "모집시작일": "2024-09-23",
//     "모집종료일": "2024-10-11",
//     "선발인원 상세내용": "12명",
//     "성적기준 상세내용": "...",
//     "소득기준 상세내용": "...",
//     "지원내역 상세내용": "1인당 100만원",
//     "학자금유형구분": "지역연고",
//     "홈페이지 주소": "https://...",
//     "제출서류 상세내용": "...",
//     "지역거주여부 상세내용": "..."
//   }],
//   "matchCount": 1646,
//   "page": 1,
//   "perPage": 100,
//   "totalCount": 1646
// }

/// Fields that tell apart rows sharing one 운영기관명 and 상품명 (one
/// foundation lists a 신입생 and a 재학생 round under one name).
const VARIANT_FIELDS: &[&str] = &[
    "대학구분",
    "학년구분",
    "학과구분",
    "학자금유형구분",
    "모집시작일",
];

/// Gives every row that shares its `운영기관명_상품명` id with another row a
/// distinct id, independent of file order (audit M1): rows sorted by their
/// [`VARIANT_FIELDS`] (then content), the first keeps the plain id so the
/// programme and the users' saved rows stay attached, the others get
/// `#` plus a key of those fields. Rows equal in every variant field are
/// told apart by content hash and logged.
fn disambiguate_colliding_ids(records: &mut [RawRecord]) {
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (index, record) in records.iter().enumerate() {
        groups
            .entry(record.source_id.clone())
            .or_default()
            .push(index);
    }
    for (id, mut members) in groups.into_iter().filter(|(_, m)| m.len() > 1) {
        let variant = |record: &RawRecord| -> String {
            VARIANT_FIELDS
                .iter()
                .map(|field| record.payload[*field].as_str().unwrap_or("").trim())
                .collect::<Vec<_>>()
                .join("|")
        };
        members.sort_by_key(|&i| (variant(&records[i]), records[i].content_hash.clone()));
        let mut used = std::collections::HashSet::new();
        for (rank, &i) in members.iter().enumerate() {
            if rank == 0 {
                used.insert(id.clone());
                continue;
            }
            let key = &content_hash(&serde_json::json!(variant(&records[i])))[..12];
            let mut new_id = format!("{id}#{key}");
            if !used.insert(new_id.clone()) {
                warn!(source = "scholarship", id = %id, "rows equal in every variant field; told apart by content");
                new_id = format!(
                    "{id}#{}",
                    &records[i].content_hash[..12.min(records[i].content_hash.len())]
                );
                used.insert(new_id.clone());
            }
            records[i].source_id = new_id;
        }
        warn!(source = "scholarship", id = %id, rows = members.len(), "rows share one name; ids made distinct");
    }
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct ScholarshipApiResponse {
    data: Vec<Value>,
    /// Rows in the whole dataset; `matchCount` is the paginated subset.
    totalCount: Option<u64>,
    matchCount: Option<u64>,
    #[allow(dead_code)]
    currentCount: Option<u64>,
    #[allow(dead_code)]
    page: Option<u64>,
    /// Rows per page the provider actually served; may be below the request.
    perPage: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Audit M1: two rows with one 운영기관명 and 상품명 are two programmes,
    /// whatever the file order; the first by their distinguishing fields
    /// keeps the plain id.
    #[test]
    fn colliding_names_get_distinct_ids_independent_of_order() {
        let row = |years: &str, end: &str| {
            let item = json!({"운영기관명": "A재단", "상품명": "희망장학금", "학년구분": years,
                "모집종료일": end});
            RawRecord {
                source_id: "A재단_희망장학금".into(),
                content_hash: content_hash(&item),
                payload: item,
            }
        };
        let other = RawRecord {
            source_id: "B재단_꿈장학금".into(),
            payload: json!({"운영기관명": "B재단", "상품명": "꿈장학금"}),
            content_hash: "x".into(),
        };
        let mut a = vec![
            row("대학신입생", "2026-05-31"),
            row("대학3학기", "2026-11-30"),
            other.clone(),
        ];
        let mut b = vec![
            other,
            row("대학3학기", "2026-11-30"),
            row("대학신입생", "2026-05-31"),
        ];
        disambiguate_colliding_ids(&mut a);
        disambiguate_colliding_ids(&mut b);
        let ids = |records: &[RawRecord]| {
            let mut ids: Vec<(String, String)> = records
                .iter()
                .map(|r| (r.source_id.clone(), r.payload["모집종료일"].to_string()))
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(ids(&a), ids(&b));
        let distinct: std::collections::HashSet<_> = a.iter().map(|r| &r.source_id).collect();
        assert_eq!(distinct.len(), 3);
        assert!(a.iter().any(|r| r.source_id == "A재단_희망장학금"));
        assert!(a.iter().any(|r| r.source_id == "B재단_꿈장학금"));
    }

    fn model(fields: &[&str]) -> Value {
        let properties: serde_json::Map<String, Value> = fields
            .iter()
            .map(|field| (field.to_string(), json!({"type": "string"})))
            .collect();
        json!({"type": "object", "properties": properties})
    }

    /// Shape of the live listing (2026-10-17): 71 dated files under one id.
    #[test]
    fn newest_listed_file_with_the_required_schema_wins() {
        let full = model(&["상품명", "운영기관명", "모집시작일", "모집종료일", "번호"]);
        let docs = json!({
            "paths": {
                PINNED_PATH: {"get": {"summary": "한국장학재단_학자금지원정보(대학생)_20250220"}},
                "/15028252/v1/uddi:8678d609-2cc1-4c28-bf49-f5f77a43a2f2": {"get": {"summary": "한국장학재단_학자금지원정보(대학생)_20260910"}},
                "/15028252/v1/uddi:aaaaaaaa-2cc1-4c28-bf49-f5f77a43a2f2": {"get": {"summary": "한국장학재단_학자금지원정보(대학생)_20261001"}},
                "/15028252/v1/uddi:bbbbbbbb-2cc1-4c28-bf49-f5f77a43a2f2": {"get": {"summary": "다른 데이터_20271231"}},
                "/99999999/v1/uddi:cccccccc-2cc1-4c28-bf49-f5f77a43a2f2": {"get": {"summary": "한국장학재단_학자금지원정보(대학생)_20281231"}}
            },
            "definitions": {
                "uddi:d0fb69e9-e143-412d-9fe0-b0d87a16f3ff_model": full,
                "uddi:8678d609-2cc1-4c28-bf49-f5f77a43a2f2_model": full,
                // Newer, but the schema lost 모집종료일: never chosen.
                "uddi:aaaaaaaa-2cc1-4c28-bf49-f5f77a43a2f2_model": model(&["상품명", "운영기관명", "모집시작일"]),
                "uddi:bbbbbbbb-2cc1-4c28-bf49-f5f77a43a2f2_model": full,
                "uddi:cccccccc-2cc1-4c28-bf49-f5f77a43a2f2_model": full
            }
        });
        assert_eq!(
            latest_dataset(&docs),
            Some((
                "/15028252/v1/uddi:8678d609-2cc1-4c28-bf49-f5f77a43a2f2".to_string(),
                "20260910".to_string()
            ))
        );
        assert_eq!(latest_dataset(&json!({"paths": {}})), None);
    }

    const NEWEST: &str = "/15028252/v1/uddi:aaaaaaaa-2cc1-4c28-bf49-f5f77a43a2f2";
    const NEXT: &str = "/15028252/v1/uddi:8678d609-2cc1-4c28-bf49-f5f77a43a2f2";
    const OLDER: &str = "/15028252/v1/uddi:29173521-fc01-40c0-b5c8-a557b0953c6e";

    fn listing() -> Vec<(String, String)> {
        vec![
            (NEWEST.into(), "20261001".into()),
            (NEXT.into(), "20260910".into()),
            (OLDER.into(), "20250819".into()),
            (PINNED_PATH.into(), PINNED_DATE.into()),
        ]
    }

    #[test]
    fn plan_never_goes_back_to_an_older_file_once_a_newer_one_was_used() {
        let (attempts, pinned) = plan_attempts(&listing(), None);
        assert!(pinned, "never switched: the pinned file is the last resort");
        assert_eq!(
            attempts.iter().map(|a| a.date.as_str()).collect::<Vec<_>>(),
            vec!["20261001", "20260910", "20250819"]
        );
        let last = (NEXT.to_string(), "20260910".to_string());
        let (attempts, pinned) = plan_attempts(&listing(), Some(&last));
        assert!(!pinned, "after a switch the pinned 2025 file is never used");
        assert_eq!(
            attempts
                .iter()
                .map(|a| (a.date.as_str(), a.require_future, a.reason))
                .collect::<Vec<_>>(),
            vec![
                ("20261001", true, "newest_valid_dataset"),
                ("20260910", false, "last_used_dataset"),
            ]
        );
        // Listing unavailable: only the last used file.
        let (attempts, pinned) = plan_attempts(&[], Some(&last));
        assert_eq!((attempts.len(), pinned), (1, false));
    }

    fn fetch_of(ends: &[&str]) -> SourceFetch {
        SourceFetch::complete(
            ends.iter()
                .enumerate()
                .map(|(n, end)| RawRecord {
                    source_id: format!("기관_장학금{n}"),
                    content_hash: "h".into(),
                    payload: json!({"상품명": "장학금", "운영기관명": "기관", "모집종료일": end}),
                })
                .collect(),
        )
    }

    /// `files`: URL suffix -> deadlines served (`None` = fetch error).
    async fn simulate(
        files: Vec<(&'static str, Option<Vec<&'static str>>)>,
        last_used: Option<(String, String)>,
    ) -> (Result<DatasetChoice>, Vec<String>) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        let (attempts, pinned) = plan_attempts(&listing(), last_used.as_ref());
        let log = calls.clone();
        let files = std::sync::Arc::new(files);
        let result = run_plan(
            attempts,
            pinned,
            "newer_datasets_not_usable",
            today,
            |url: String| {
                log.lock().unwrap().push(url.clone());
                let files = files.clone();
                async move {
                    let served = files
                        .iter()
                        .find(|(suffix, _)| url.ends_with(suffix))
                        .and_then(|(_, ends)| ends.clone());
                    match served {
                        Some(ends) => Ok(fetch_of(&ends)),
                        None => anyhow::bail!("HTTP 500"),
                    }
                }
            },
        )
        .await
        .map(|(_, choice)| choice);
        let calls = calls.lock().unwrap().clone();
        (result, calls)
    }

    #[tokio::test]
    async fn a_failing_newest_file_falls_back_to_the_next_newest_valid_one() {
        let (choice, _) = simulate(
            vec![
                (NEWEST, Some(vec!["2025-12-31"])),
                (NEXT, Some(vec!["2026-11-30"])),
            ],
            None,
        )
        .await;
        let choice = choice.unwrap();
        assert_eq!(
            (choice.dataset_date.as_str(), choice.pinned, choice.reason),
            ("20260910", false, "next_newest_valid_dataset")
        );
    }

    #[tokio::test]
    async fn after_a_switch_the_last_used_file_wins_over_the_pinned_one() {
        let last = Some((NEXT.to_string(), "20260910".to_string()));
        // Newest broken; the last used file now has only past deadlines but
        // is still the catalogue users have: re-read it, never the pinned.
        let (choice, calls) = simulate(
            vec![(NEWEST, None), (NEXT, Some(vec!["2026-09-30"]))],
            last.clone(),
        )
        .await;
        let choice = choice.unwrap();
        assert_eq!(
            (choice.dataset_date.as_str(), choice.reason),
            ("20260910", "last_used_dataset")
        );
        assert!(!calls.iter().any(|url| url == BASE_URL));

        // Nothing usable: fail (no upsert), and the pinned file is not read.
        let (choice, calls) = simulate(vec![(NEWEST, None), (NEXT, None)], last).await;
        let error = choice.unwrap_err().to_string();
        assert!(error.contains("keeping the current catalogue"), "{error}");
        assert!(!calls.iter().any(|url| url == BASE_URL), "{calls:?}");
    }

    #[tokio::test]
    async fn the_pinned_file_is_used_only_before_any_switch() {
        let (choice, calls) = simulate(
            vec![
                (NEWEST, None),
                (NEXT, Some(vec![])),
                (OLDER, Some(vec!["2025-09-01"])),
                (
                    "uddi:d0fb69e9-e143-412d-9fe0-b0d87a16f3ff",
                    Some(vec!["2025-12-31"]),
                ),
            ],
            None,
        )
        .await;
        let choice = choice.unwrap();
        assert!(choice.pinned);
        assert_eq!(calls.last().map(String::as_str), Some(BASE_URL));
    }

    #[test]
    fn a_new_file_needs_rows_and_a_future_deadline() {
        let record = |end: &str| RawRecord {
            source_id: "기관_장학금".into(),
            content_hash: "h".into(),
            payload: json!({"상품명": "장학금", "운영기관명": "기관", "모집종료일": end}),
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        assert_eq!(validate_dataset(&[], today), Err("latest_dataset_empty"));
        assert_eq!(
            validate_dataset(&[record("2025-12-31"), record("상시")], today),
            Err("latest_dataset_has_no_future_deadline")
        );
        assert_eq!(
            validate_dataset(&[record("2025-12-31"), record("2026-10-17")], today),
            Ok(())
        );
    }
}
