pub mod benefit_pages;
pub mod brand_catalog;
pub mod financial;
pub mod gov_benefits;
pub(crate) mod http;
pub mod live_page;
pub mod local_welfare;
pub mod national_welfare;
pub mod official_seeds;
pub mod scholarship;
pub(crate) mod source_access;
pub mod youth_center;

use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;

/// A raw record fetched from an external data source.
#[derive(Debug, Clone)]
pub struct RawRecord {
    /// Stable identifier within the source (e.g. service code)
    pub source_id: String,
    /// Raw JSON payload as returned by the API
    pub payload: Value,
    /// Hex digest of the canonical JSON bytes of the whole payload. Sources
    /// derive their fallback ids (`gov_unknown_…`) from it, so it must keep
    /// this definition: a new id would orphan users' rows of the programme.
    /// Change detection does not read it; the pipeline derives its own hash
    /// with [`record_hash`].
    pub content_hash: String,
}

/// Records fetched from a source and what became of each part of its
/// catalogue.
#[derive(Debug, Clone, Default)]
pub struct SourceFetch {
    pub records: Vec<RawRecord>,
    /// Records the provider returned before any client-side filter (the
    /// youth filters of the welfare and financial sources). Zero means the
    /// provider itself returned nothing, which the pipeline fails unless the
    /// source may legitimately be empty; `records` empty with a non-zero
    /// count means every record was filtered out.
    pub upstream_seen: usize,
    /// One outcome per part of the catalogue the fetch read. A source read
    /// as one part has a single [`PartScope::Whole`] part; a source read in
    /// parts names each by the source-id prefix of its records. Lifecycle
    /// reconciliation never counts a programme of an unsettled part as
    /// missing; an unsettled whole skips reconciliation altogether.
    pub parts: Vec<PartOutcome>,
    /// Ids of rows the provider listed that the source's own filter left
    /// out. The pipeline names any of them that is still an active programme
    /// ("was kept, now filtered out"), which would otherwise look the same
    /// as a product the provider removed.
    pub filtered_out: Vec<String>,
    /// Ids of rows the provider listed that the source set aside as
    /// defective (an over-long field, a missing kind field): listed by the
    /// provider, not kept by us, like `filtered_out`.
    pub set_aside_ids: Vec<String>,
}

/// Which records a part covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartScope {
    /// The whole catalogue (a source read as one part).
    Whole,
    /// Every record whose source id starts with this prefix, and no other.
    /// Never empty: the pipeline rejects an empty prefix, which would cover
    /// every programme of the source.
    Prefix(String),
}

/// What a fetch could say about one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartState {
    /// Read to the provider's own total.
    Settled,
    /// The provider answered that the part has no records. The same answer
    /// is what an outage can look like, so the pipeline treats it as
    /// unsettled while programmes of the part are still active.
    Empty,
    /// Not read to the end: the part's stored programmes are left alone.
    Unsettled(Unsettled),
}

/// Why a part is not settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsettled {
    /// Stable reason code (`short_read`, `transport`, `schema_changed`, …).
    pub code: &'static str,
    /// Fixed text and counts: never a URL, a key or provider free text.
    pub detail: String,
}

/// The outcome of one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartOutcome {
    pub scope: PartScope,
    pub state: PartState,
    /// Rows the provider returned for the part, before the source filter.
    pub seen: usize,
    /// Records the part contributed to [`SourceFetch::records`].
    pub kept: usize,
    /// Rows the source set aside as defective (a row without its id, a
    /// repeated id, an over-long field).
    pub set_aside: usize,
}

/// Reason code of a whole-source fetch that did not reach the provider's
/// total (see [`ensure_complete`]).
pub const SHORT_READ: &str = "short_read";

impl SourceFetch {
    /// A fetch with no provider total to miss and no client-side filter:
    /// single-request or fully enumerated sources.
    pub fn complete(records: Vec<RawRecord>) -> Self {
        Self::unfiltered(records, true)
    }

    /// A fetch that keeps every record the provider returned.
    pub fn unfiltered(records: Vec<RawRecord>, complete: bool) -> Self {
        let seen = records.len();
        Self::filtered(records, seen, complete)
    }

    /// A fetch read as one part, `upstream_seen` rows before the source's
    /// filter. `complete` false: the page loop missed the provider's total
    /// ([`ensure_complete`]); the records are ingested, reconciliation is
    /// skipped.
    pub fn filtered(records: Vec<RawRecord>, upstream_seen: usize, complete: bool) -> Self {
        let state = if complete {
            PartState::Settled
        } else {
            PartState::Unsettled(Unsettled {
                code: SHORT_READ,
                detail: "the page loop did not reach the provider's total".into(),
            })
        };
        Self {
            parts: vec![PartOutcome {
                scope: PartScope::Whole,
                state,
                seen: upstream_seen,
                kept: records.len(),
                set_aside: 0,
            }],
            upstream_seen,
            records,
            filtered_out: Vec::new(),
            set_aside_ids: Vec::new(),
        }
    }

    /// No part covering the whole catalogue is unsettled.
    pub fn whole_settled(&self) -> bool {
        !self.parts.iter().any(|part| {
            part.scope == PartScope::Whole && matches!(part.state, PartState::Unsettled(_))
        })
    }
}

/// Every data source must implement this trait.
/// Uses RPITIT (return-position impl Trait in traits, stable since Rust 1.75).
pub trait DataSource: Send + Sync {
    /// Human-readable name used in logs and ingestion_runs rows.
    fn name(&self) -> &'static str;

    /// Fetch all records from the source.
    fn fetch_all(&self) -> impl std::future::Future<Output = Result<SourceFetch>> + Send;
}

/// Top-level payload keys that must not count as a change of the record:
/// view counters, which a source changes on every read, and the 보조금24
/// `detail` row ([`gov_benefits::DETAIL_KEY`]), side data the normalizer
/// never reads (only the link resolver does, from `source_snapshots`). They
/// stay in `raw_payload`; only the change hash ([`record_hash`]), the AI
/// extraction source text and the AI extraction queue trigger ignore them.
/// Source ids never use this hash. A key that can mean an edit (`수정일시`,
/// `lastModYmd`, `lastMdfcnDt`) or states the offer (`sprtSclCnt`, the
/// number of places) is never listed here.
///
/// The SQL function `ingestion_stable_payload` (migrations
/// `20261021000001_ai_queue_ignores_view_counters.sql`,
/// `20261023000102_stable_payload_ignores_gov24_detail.sql`) carries the
/// same list; `pipeline::volatile_hash_tests` fails when the two drift.
pub fn volatile_keys(source: &str) -> &'static [&'static str] {
    match source {
        "gov_benefits" => &["조회수", gov_benefits::DETAIL_KEY],
        "youth_center" => &["inqCnt"],
        "local_welfare" => &["inqNum"],
        "national_welfare" => &["inqNum", national_welfare::DETAIL_KEY],
        _ => &[],
    }
}

/// Whether `key` is a volatile key of any source (for code that sees a
/// payload without its source name).
pub fn is_volatile_key_of_any_source(key: &str) -> bool {
    [
        "gov_benefits",
        "youth_center",
        "local_welfare",
        "national_welfare",
    ]
    .iter()
    .any(|source| volatile_keys(source).contains(&key))
}

/// The change hash of a source record: [`content_hash`] of the payload
/// without the source's [`volatile_keys`].
pub fn record_hash(source: &str, payload: &Value) -> String {
    let keys = volatile_keys(source);
    match payload {
        Value::Object(map) if keys.iter().any(|key| map.contains_key(*key)) => {
            let mut stable = map.clone();
            for key in keys {
                stable.remove(*key);
            }
            content_hash(&Value::Object(stable))
        }
        _ => content_hash(payload),
    }
}

/// Compute a content hash for a JSON value.
/// Serialises to canonical (sorted-key) JSON then applies FNV-1a (128-bit).
/// `{source}_{servId}`, or `{source}_unknown_{content hash}` for a record
/// without an id, so two such records never overwrite each other (audit L1;
/// the fallback every other source uses).
pub(crate) fn welfare_source_id(source: &str, serv_id: Option<&str>, hash: &str) -> String {
    match serv_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => format!("{source}_{id}"),
        None => format!("{source}_unknown_{}", &hash[..hash.len().min(16)]),
    }
}

/// An XML element read through a flattened serde map, as plain JSON: the
/// element's text (`$text`/`$value`) for a leaf, the map otherwise.
pub(crate) fn xml_text(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.len() == 1 => map
            .get("$text")
            .or_else(|| map.get("$value"))
            .cloned()
            .unwrap_or_else(|| value.clone()),
        other => other.clone(),
    }
}

pub fn content_hash(v: &Value) -> String {
    let canonical = canonical_value(v);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    fnv_hash_hex(&bytes)
}

fn canonical_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(k, val)| (k.clone(), canonical_value(val)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

pub fn fnv_hash_hex(data: &[u8]) -> String {
    // FNV-1a with two different seeds → 128-bit collision resistance.
    // Replace with sha2 crate if cryptographic strength is required.
    let h1 = fnv1a(data, 0xcbf29ce484222325_u64);
    let h2 = fnv1a(data, 0x14650fb0739d0383_u64);
    format!("{:016x}{:016x}", h1, h2)
}

fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let prime: u64 = 0x00000100000001B3;
    let mut hash = seed;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(prime);
    }
    hash
}

/// Whether the page loop delivered the provider's own total. A provider that
/// silently caps the page size (50 instead of the requested 100) ends the
/// loop after one short page; a provider whose `totCount` includes rows it
/// never returns (soft-deleted, cached) never reaches its total at all. In
/// both cases the records are real and are ingested, but the fetch must not
/// drive lifecycle reconciliation, which would close every program it never
/// saw. `collected` counts records before any client-side filtering. Without
/// a provider total the collection is accepted and the gap is logged.
pub(crate) fn ensure_complete(source: &str, collected: usize, total: Option<u64>) -> bool {
    match total {
        Some(total) if (collected as u64) < total => {
            tracing::warn!(
                source,
                collected,
                total,
                "pagination incomplete; records ingested, lifecycle reconciliation skipped"
            );
            false
        }
        Some(_) => true,
        None => {
            tracing::warn!(
                source,
                collected,
                "provider reported no total; completeness unverified"
            );
            true
        }
    }
}

/// Rows per page the provider actually served. The requested size is only an
/// upper bound: public APIs cap it without an error, so the loop must stop on
/// the provider's own page size (echoed when available, otherwise observed).
pub(crate) fn effective_page_size(echoed: Option<u64>, fetched: usize, requested: u32) -> u64 {
    echoed
        .filter(|size| *size > 0)
        .unwrap_or(fetched as u64)
        .min(u64::from(requested))
        .max(1)
}

#[cfg(test)]
mod record_hash_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn view_counters_do_not_change_the_record_hash() {
        let cases = [
            ("gov_benefits", "조회수", json!(2843), json!(2844)),
            ("youth_center", "inqCnt", json!("72"), json!("73")),
            ("local_welfare", "inqNum", json!(204), json!(205)),
            ("national_welfare", "inqNum", json!(3639), json!(3700)),
        ];
        for (source, key, before, after) in cases {
            let mut old = json!({"name": "청년 월세 지원", "수정일시": "20260807162419"});
            let mut new = old.clone();
            old[key] = before;
            new[key] = after;
            assert_eq!(
                record_hash(source, &old),
                record_hash(source, &new),
                "{source}"
            );
            assert_ne!(content_hash(&old), content_hash(&new), "{source}");
        }
    }

    #[test]
    fn edits_and_offer_fields_still_change_the_record_hash() {
        let base = json!({"서비스명": "청년 월세 지원", "수정일시": "20260807162419", "조회수": 1});
        let mut edited = base.clone();
        edited["수정일시"] = json!("20261019090000");
        assert_ne!(
            record_hash("gov_benefits", &base),
            record_hash("gov_benefits", &edited)
        );

        let youth =
            json!({"plcyNm": "청년 지원", "sprtSclCnt": "0", "lastMdfcnDt": "2026-04-23 16:38:26"});
        for (key, value) in [("sprtSclCnt", "20"), ("lastMdfcnDt", "2026-10-20 09:00:00")] {
            let mut changed = youth.clone();
            changed[key] = json!(value);
            assert_ne!(
                record_hash("youth_center", &youth),
                record_hash("youth_center", &changed),
                "{key}"
            );
        }
        // A counter of another source is content here.
        let scholarship = json!({"상품명": "장학금", "inqCnt": "1"});
        let mut changed = scholarship.clone();
        changed["inqCnt"] = json!("2");
        assert_ne!(
            record_hash("scholarship", &scholarship),
            record_hash("scholarship", &changed)
        );
    }

    /// Fallback source ids (`gov_unknown_{hash[..16]}`, `yc_unknown_…`,
    /// `ks_unknown_…`) come from `content_hash` of the whole payload. Pinned
    /// so the id of an existing programme never changes with the hash used
    /// for change detection.
    #[test]
    fn the_id_hash_is_the_whole_payload_hash_and_does_not_change() {
        let payload = json!({"서비스명": "청년 월세 지원", "조회수": 2843});
        assert_eq!(content_hash(&payload), "df2108203d9b4976e4756e3a7546a4e0");
        assert_ne!(
            content_hash(&payload),
            record_hash("gov_benefits", &payload)
        );
    }

    #[test]
    fn a_payload_without_volatile_keys_keeps_its_previous_hash() {
        let payload = json!({"상품명": "장학금", "운영기관명": "재단"});
        assert_eq!(record_hash("scholarship", &payload), content_hash(&payload));
        assert_eq!(
            record_hash("gov_benefits", &payload),
            content_hash(&payload)
        );
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    #[test]
    fn truncated_collection_is_flagged_incomplete_not_failed() {
        assert!(!ensure_complete("fixture", 50, Some(1828)));
        assert!(!ensure_complete("fixture", 1827, Some(1828)));
        assert!(ensure_complete("fixture", 1828, Some(1828)));
        // Deletions during the fetch may leave more records than the total.
        assert!(ensure_complete("fixture", 1830, Some(1828)));
        assert!(ensure_complete("fixture", 0, Some(0)));
        assert!(ensure_complete("fixture", 7, None));
    }

    #[test]
    fn provider_page_size_overrides_the_requested_one() {
        assert_eq!(effective_page_size(Some(50), 50, 100), 50);
        assert_eq!(effective_page_size(None, 50, 100), 50);
        assert_eq!(effective_page_size(Some(0), 100, 100), 100);
        assert_eq!(effective_page_size(Some(500), 100, 100), 100);
        assert_eq!(effective_page_size(None, 0, 100), 1);
    }
}

/// Returns true if a comma-separated life-stage array field (e.g.
/// `"청년,중장년,노년"` or `"노년, 아동, 청소년, 청년"`) contains the exact
/// token "청년" (youth).
///
/// Shared by `national_welfare` (`lifeArray`) and `local_welfare`
/// (`lifeNmArray`) to keep non-youth welfare records (노인/영유아/장애인 전용
/// 등) out of the recommendation pool. Token-based equality (not raw
/// substring) so the check stays correct even if the API's life-stage
/// vocabulary grows — e.g. "청소년" (teen) must never match "청년" (youth).
pub fn life_stage_includes_youth(field: Option<&str>) -> bool {
    field
        .map(|s| s.split(',').any(|tok| tok.trim() == "청년"))
        .unwrap_or(false)
}

#[cfg(test)]
mod life_stage_tests {
    use super::*;

    #[test]
    fn none_is_not_youth() {
        assert!(!life_stage_includes_youth(None));
    }

    #[test]
    fn empty_string_is_not_youth() {
        assert!(!life_stage_includes_youth(Some("")));
    }

    #[test]
    fn exact_youth_token_matches() {
        assert!(life_stage_includes_youth(Some("청년")));
    }

    #[test]
    fn youth_among_other_tokens_matches_no_space() {
        assert!(life_stage_includes_youth(Some("청년,중장년,노년")));
    }

    #[test]
    fn youth_among_other_tokens_matches_with_space() {
        assert!(life_stage_includes_youth(Some("노년, 아동, 청소년, 청년")));
    }

    #[test]
    fn teen_only_does_not_match_youth() {
        // "청소년" (teen) must never be conflated with "청년" (youth).
        assert!(!life_stage_includes_youth(Some("영유아,아동,청소년")));
    }

    #[test]
    fn elderly_and_infant_only_does_not_match() {
        assert!(!life_stage_includes_youth(Some("영유아,노년")));
    }

    #[test]
    fn pregnancy_only_does_not_match() {
        assert!(!life_stage_includes_youth(Some("임신 · 출산")));
    }
}

/// Search-engine APIs are no data source: the Naver Search API terms (2.3)
/// forbid copying, storing or caching its data and anything derived from
/// it and feeding it to AI, and Kakao's developer policy forbids entering
/// it into a directory (`.reports/corp-kr-routes.md` R7, R8). No worker code
/// may call one; official domains, sitemaps and feeds are the only
/// discovery channels.
#[cfg(test)]
mod no_search_engine_api {
    #[test]
    fn no_worker_code_calls_a_search_engine_api() {
        const HOSTS: &[&str] = &[
            "openapi.naver.com",
            "dapi.kakao.com",
            "customsearch.googleapis.com",
            "api.bing.microsoft.com",
        ];
        fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for host in HOSTS {
                        if text.contains(host) && !path.ends_with("sources/mod.rs") {
                            found.push(format!("{}: {host}", path.display()));
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        assert!(found.is_empty(), "search-engine API calls: {found:?}");
    }
}

#[cfg(test)]
mod robots_legacy;
