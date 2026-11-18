//! Reviewed eligibility conditions (`program_condition_reviews`): the
//! all-or-nothing importer and the rolling-window confirmation job.
//!
//! Import lines are [`ReviewInput`] records (the pilot's `reviews.jsonl`
//! shape plus `reviewer`; see `mazimi_core::condition_review::pilot`). Every
//! line must carry `text_hash`, the hash of the text the curators read; it
//! must equal the hash of the programme's CURRENT `raw_payload` (else
//! `text_changed`), every rule leaf must use a supported field, operator and
//! value, and every quote must be verbatim. Code overrides (`code_overrides`)
//! must name a code rule the programme has now and provably widen it. If any
//! line is rejected nothing is written; `--dry-run` never writes.
//!
//! Rolling confirmation: a 온통청년 review whose window is `rolling` or
//! `unknown` and whose stored source code is 상시 (`aplyPrdSeCd` 0057002) is
//! confirmed from the daily sync's stored payload, with no network request.
//! It counts as confirmed when the latest successful 온통청년 sync that
//! returned the record (`ingestion_items` `upserted` or `unchanged`) finished
//! within [`SYNC_FRESHNESS_HOURS`]: `rolling`, confirmed at that sync's
//! finish time (open for [`ROLLING_CONFIRMATION_TTL_DAYS`] days). When the
//! latest sync failed or is older than that, the job is degraded and
//! confirms nothing; existing confirmations age out on their own. A record
//! no fresh sync returned stays as it is (`not_seen_recently`); an
//! unconfirmed rolling window is never "open now". A record whose synced
//! code is no longer 0057002 leaves "open now" on that sync, before its
//! confirmation expires: `aplyPrdSeCd` is a hashed field, so the review is
//! stale and the list reads no window from it (it is also no candidate).
//!
//! What a confirmation proves: the daily sync re-read the same official
//! 온통청년 record (Open API `getPlcy`) recently and its code is still
//! 0057002. It is not a second, independent source: a wrong 상시 code
//! upstream is confirmed as rolling, and nothing here cross-checks the
//! notice text or the agency's own page. It replaces a per-policy Open API
//! re-read, which served the same record and was refused (HTTP 403) after
//! about 14 calls in one run. Writes are off unless
//! `ROLLING_WINDOW_CONFIRMATION_ENABLED`.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use mazimi_core::condition_review::{
    self,
    store::{self, Confirmation, ReviewInput, Written},
    ROLLING_CONFIRMATION_TTL_DAYS, YOUTH_ROLLING_CODE,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Connection, PgPool};

#[derive(Debug, Default, Serialize)]
pub struct ImportSummary {
    pub dry_run: bool,
    /// Whether the writes were committed.
    pub applied: bool,
    pub lines: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub rejected: usize,
    pub rejected_by_reason: BTreeMap<String, usize>,
    pub accepted_by_status: BTreeMap<String, usize>,
    /// Code overrides of accepted lines, by `<target>:<action>`
    /// (`S-age:widen`, `C-job:drop`, …).
    pub overrides_by_target: BTreeMap<String, usize>,
    /// Rejected lines (1-based line number, source id, reason). No source
    /// text is echoed.
    pub rejections: Vec<LineRejection>,
}

#[derive(Debug, Serialize)]
pub struct LineRejection {
    pub line: usize,
    pub source_id: Option<String>,
    pub reason: String,
}

/// Import `text` (JSON lines) in a transaction of `connection`.
pub async fn import_on(
    connection: &mut sqlx::PgConnection,
    text: &str,
    actor: &str,
    dry_run: bool,
) -> Result<ImportSummary, sqlx::Error> {
    let mut summary = ImportSummary {
        dry_run,
        ..ImportSummary::default()
    };
    let mut seen = HashSet::new();
    let mut tx = connection.begin().await?;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        summary.lines += 1;
        let mut reject = |source_id: Option<String>, reason: String| {
            *summary
                .rejected_by_reason
                .entry(reason.clone())
                .or_default() += 1;
            summary.rejected += 1;
            summary.rejections.push(LineRejection {
                line: index + 1,
                source_id,
                reason,
            });
        };
        let review: ReviewInput = match serde_json::from_str(line) {
            Ok(review) => review,
            Err(_) => {
                reject(None, "invalid_json".into());
                continue;
            }
        };
        let key = (
            review.source_type.trim().to_owned(),
            review.source_id.trim().to_owned(),
        );
        if !seen.insert(key) {
            reject(Some(review.source_id), "duplicate".into());
            continue;
        }
        match store::upsert(&mut tx, &review, actor, true).await? {
            Ok(written) => {
                *summary
                    .accepted_by_status
                    .entry(review.status.trim().to_owned())
                    .or_default() += 1;
                for o in &review.code_overrides {
                    let text = |key: &str| o[key].as_str().unwrap_or_default().trim().to_owned();
                    *summary
                        .overrides_by_target
                        .entry(format!("{}:{}", text("target"), text("action")))
                        .or_default() += 1;
                }
                match written {
                    Written::Inserted => summary.inserted += 1,
                    Written::Updated => summary.updated += 1,
                    Written::Unchanged => summary.unchanged += 1,
                }
            }
            Err(rejection) => {
                let reason = serde_json::to_value(rejection)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{rejection:?}"));
                reject(Some(review.source_id), reason);
            }
        }
    }
    if dry_run || summary.rejected > 0 {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
        summary.applied = true;
    }
    Ok(summary)
}

// ── Rolling-window confirmation ────────────────────────────────────────────

/// Log actor of the confirmation job.
pub const CONFIRMATION_ACTOR: &str = "job:rolling_window_confirmation";
const DEFAULT_MAX_PROGRAMS: u64 = 500;
/// A record counts as re-read only when a successful 온통청년 sync that saw it
/// finished within this many hours; an older or failed sync confirms nothing.
pub const SYNC_FRESHNESS_HOURS: i64 = 36;
/// `ingestion_runs.source_name` of the daily 온통청년 sync.
const YOUTH_SYNC_SOURCE: &str = "youth_center";
/// Run statuses whose stored payloads are the fetched records.
const SYNC_OK_STATUSES: &[&str] = &["success", "success_with_rejections"];

#[derive(Debug, Clone)]
pub struct ConfirmationConfig {
    pub max_programs: u64,
}

/// `None` unless `ROLLING_WINDOW_CONFIRMATION_ENABLED` is true (the job
/// writes confirmations only when enabled; it makes no network request).
pub fn confirmation_config_from_env() -> Result<Option<ConfirmationConfig>> {
    let enabled = std::env::var("ROLLING_WINDOW_CONFIRMATION_ENABLED")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(enabled.as_str(), "true" | "1" | "yes" | "on") {
        return Ok(None);
    }
    let max_programs = match std::env::var("ROLLING_WINDOW_CONFIRMATION_MAX") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .context("ROLLING_WINDOW_CONFIRMATION_MAX must be an integer")?,
        _ => DEFAULT_MAX_PROGRAMS,
    };
    if !(1..=1_500).contains(&max_programs) {
        bail!("ROLLING_WINDOW_CONFIRMATION_MAX must be between 1 and 1500");
    }
    Ok(Some(ConfirmationConfig { max_programs }))
}

#[derive(Debug, Default, Serialize)]
pub struct ConfirmationSummary {
    pub dry_run: bool,
    /// When the latest finished 온통청년 sync ended, and how.
    pub sync_finished_at: Option<DateTime<Utc>>,
    pub sync_status: Option<String>,
    /// Why nothing was confirmed: the latest sync failed, or none finished
    /// within [`SYNC_FRESHNESS_HOURS`]. `None` when the sync is fresh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    pub candidates: usize,
    /// The review text changed since it was written: not re-checked.
    pub stale: usize,
    /// An `unknown` window whose curators labelled window text they could not
    /// resolve (an event date, a round): never promoted on the code alone.
    pub window_text_unresolved: usize,
    pub confirmed_rolling: usize,
    /// No successful sync within [`SYNC_FRESHNESS_HOURS`] returned the
    /// record (it left the official list, or was quarantined): not confirmed.
    pub not_seen_recently: usize,
}

/// (source_type, source_id, text_hash, hashed fields, coverage labels of an
/// `unknown` window, when the latest fresh successful sync that saw the
/// record finished).
pub type Candidate = (
    String,
    String,
    String,
    Option<Value>,
    Option<Value>,
    Option<DateTime<Utc>>,
);

/// SQL twin of [`condition_review::text_hash`] over `{alias}raw_payload`:
/// each hashed field as text (a JSON string as is, null or missing as
/// empty, anything else as its JSON text), joined with U+001F, SHA-256 in
/// lowercase hex. NULL for a source without reviewed fields. 온통청년
/// fields are strings, where the two agree byte for byte; the Rust check in
/// [`confirm_candidates`] stays as the guard.
fn review_text_hash_sql(alias: &str) -> String {
    let fields = condition_review::hashed_fields("youth_center")
        .expect("youth_center is supported")
        .into_iter()
        .map(|field| {
            let value = format!("{alias}raw_payload->'{field}'");
            format!(
                "COALESCE(CASE jsonb_typeof({value}) WHEN 'string' THEN {value} #>> '{{}}' \
                 WHEN 'null' THEN '' ELSE ({value})::text END, '')"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CASE WHEN {alias}source_type = 'youth_center' THEN \
         encode(sha256(convert_to(array_to_string(ARRAY[{fields}], chr(31)), 'UTF8')), 'hex') END"
    )
}

/// The latest finished 온통청년 sync: (status, finished_at).
pub async fn latest_sync(pool: &PgPool) -> Result<Option<(String, DateTime<Utc>)>> {
    Ok(sqlx::query_as(
        "SELECT status, finished_at FROM ingestion_runs
         WHERE source_name = $1 AND COALESCE(run_kind, 'ingestion') = 'ingestion'
           AND finished_at IS NOT NULL
         ORDER BY finished_at DESC LIMIT 1",
    )
    .bind(YOUTH_SYNC_SOURCE)
    .fetch_optional(pool)
    .await?)
}

/// Why `sync` cannot confirm anything at `now`, or `None` when it can.
pub fn sync_degraded(sync: Option<&(String, DateTime<Utc>)>, now: DateTime<Utc>) -> Option<String> {
    match sync {
        None => Some("no finished 온통청년 sync".into()),
        Some((status, _)) if !SYNC_OK_STATUSES.contains(&status.as_str()) => {
            Some(format!("latest 온통청년 sync {status}"))
        }
        Some((_, at)) if now - *at > chrono::Duration::hours(SYNC_FRESHNESS_HOURS) => Some(
            format!("latest 온통청년 sync finished more than {SYNC_FRESHNESS_HOURS} h ago"),
        ),
        Some(_) => None,
    }
}

/// Complete 온통청년 reviews whose stored source code is 상시 and whose
/// window is `rolling`, `unknown`, or `closed` by an earlier re-check (so a
/// record that reopens is seen again), least recently confirmed first, with
/// the finish time of the latest successful sync since `fresh_since` that
/// returned the record (`upserted` or `unchanged`).
/// Reviews whose text changed since they were written, and `unknown`
/// windows whose curators labelled window text, are left out here: they are
/// never confirmed, so in the result they would keep the oldest places of
/// the LIMIT and starve every checkable review behind them.
pub async fn confirmation_candidates(
    pool: &PgPool,
    limit: u64,
    fresh_since: DateTime<Utc>,
) -> Result<Vec<Candidate>> {
    Ok(sqlx::query_as::<_, Candidate>(&format!(
        "WITH fresh AS (
             SELECT id, finished_at FROM ingestion_runs
             WHERE source_name = $3 AND COALESCE(run_kind, 'ingestion') = 'ingestion'
               AND status = ANY($4) AND finished_at >= $5)
         SELECT r.source_type, r.source_id, r.text_hash, {hashed},
                CASE WHEN r.window_json->>'kind' = 'unknown' THEN r.coverage_json->'labels' END,
                seen.at
         FROM program_condition_reviews r
         JOIN current_programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         LEFT JOIN LATERAL (
             SELECT max(f.finished_at) AS at FROM fresh f
             JOIN ingestion_items i ON i.run_id = f.id AND i.source_id = r.source_id
             WHERE i.status IN ('upserted', 'unchanged')) seen ON true
         WHERE r.source_type = 'youth_center' AND r.status = 'complete'
           AND (r.window_json->>'kind' IN ('rolling', 'unknown')
                OR (r.window_json->>'kind' = 'closed'
                    AND r.window_json->>'source' = 'official_recheck'))
           AND p.raw_payload->>'aplyPrdSeCd' = $2
           AND r.text_hash = {text_hash}
           AND NOT (r.window_json->>'kind' = 'unknown' AND EXISTS (
               SELECT 1 FROM jsonb_each(CASE WHEN jsonb_typeof(r.coverage_json->'labels') = 'object'
                                             THEN r.coverage_json->'labels' ELSE '{{}}'::jsonb END) label
               WHERE label.value->>'label' = 'window'
                 AND (label.value->'prelabel') IS DISTINCT FROM 'true'::jsonb))
         ORDER BY r.window_confirmed_at ASC NULLS FIRST, r.source_id
         LIMIT $1",
        hashed = condition_review::hashed_projection_sql("p."),
        text_hash = review_text_hash_sql("p."),
    ))
    .bind(limit as i64)
    .bind(YOUTH_ROLLING_CODE)
    .bind(YOUTH_SYNC_SOURCE)
    .bind(SYNC_OK_STATUSES)
    .bind(fresh_since)
    .fetch_all(pool)
    .await?)
}

/// Whether the curators labelled a text segment (not a code prelabel) as
/// `window` in the coverage map.
fn curator_window_text(labels: Option<&Value>) -> bool {
    labels.and_then(Value::as_object).is_some_and(|labels| {
        labels.values().any(|entry| {
            entry.get("label").and_then(Value::as_str) == Some("window")
                && entry.get("prelabel").and_then(Value::as_bool) != Some(true)
        })
    })
}

/// Confirm up to `config.max_programs` candidates from the stored sync
/// payloads at `now`. No network request.
pub async fn confirm_rolling_windows(
    pool: &PgPool,
    config: &ConfirmationConfig,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConfirmationSummary> {
    let sync = latest_sync(pool).await?;
    let fresh_since = now - chrono::Duration::hours(SYNC_FRESHNESS_HOURS);
    let candidates = confirmation_candidates(pool, config.max_programs, fresh_since).await?;
    confirm_candidates(pool, sync, candidates, dry_run, now).await
}

/// Confirm the given candidates (see [`confirm_rolling_windows`]). A
/// degraded `sync` confirms nothing; otherwise a candidate a fresh
/// successful sync saw is `rolling`, confirmed at that sync's finish time.
pub async fn confirm_candidates(
    pool: &PgPool,
    sync: Option<(String, DateTime<Utc>)>,
    candidates: Vec<Candidate>,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConfirmationSummary> {
    let mut summary = ConfirmationSummary {
        dry_run,
        degraded: sync_degraded(sync.as_ref(), now),
        sync_status: sync.as_ref().map(|(status, _)| status.clone()),
        sync_finished_at: sync.as_ref().map(|(_, at)| *at),
        candidates: candidates.len(),
        ..ConfirmationSummary::default()
    };
    if summary.degraded.is_some() {
        tracing::warn!(
            ?summary,
            "rolling window confirmation degraded; nothing confirmed"
        );
        return Ok(summary);
    }
    let fresh_since = now - chrono::Duration::hours(SYNC_FRESHNESS_HOURS);
    for (source_type, source_id, stored_hash, hashed, unknown_labels, seen_at) in candidates {
        let current =
            condition_review::text_hash(&source_type, hashed.as_ref().unwrap_or(&Value::Null));
        if current.as_deref() != Some(stored_hash.as_str()) {
            summary.stale += 1;
            continue;
        }
        if curator_window_text(unknown_labels.as_ref()) {
            summary.window_text_unresolved += 1;
            continue;
        }
        let Some(seen_at) = seen_at.filter(|at| *at >= fresh_since && *at <= now) else {
            summary.not_seen_recently += 1;
            continue;
        };
        summary.confirmed_rolling += 1;
        if dry_run {
            continue;
        }
        let mut connection = pool.acquire().await?;
        store::confirm_window(
            &mut connection,
            &source_type,
            &source_id,
            &stored_hash,
            Confirmation::StillRolling,
            seen_at,
            CONFIRMATION_ACTOR,
        )
        .await?;
    }
    tracing::info!(
        ?summary,
        ttl_days = ROLLING_CONFIRMATION_TTL_DAYS,
        "rolling window confirmation complete"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazimi_core::condition_review::text_hash;
    use serde_json::json;
    use std::collections::HashMap;

    async fn connection() -> sqlx::PgConnection {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::PgConnection::connect(&url)
            .await
            .expect("isolated test database required")
    }

    fn raw(title: &str, code: &str) -> Value {
        json!({
            "plcyNm": title,
            "plcyExplnCn": "부산 거주 미취업 청년의 구직 활동을 지원",
            "plcyAplyMthdCn": "온라인 신청",
            "addAplyQlfcCndCn": "부산 거주 미취업 청년",
            "sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "34", "zipCd": "26410",
            "jobCd": "0013003", "aplyPrdSeCd": code, "aplyYmd": "",
        })
    }

    async fn programme(connection: &mut sqlx::PgConnection, source_id: &str, payload: &Value) {
        sqlx::query("INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active,raw_payload) VALUES('policy','youth_center',$1,$2,'active',true,$3)")
            .bind(source_id)
            .bind(payload["plcyNm"].as_str())
            .bind(payload)
            .execute(connection)
            .await
            .unwrap();
    }

    fn line(source_id: &str, payload: &Value, extra: Value) -> String {
        let mut base = json!({
            "source_type": "youth_center", "source_id": source_id,
            "text_hash": text_hash("youth_center", payload).unwrap(),
            "status": "complete",
            "rules": [{"id": "R1", "field": "employment_status", "op": "in",
                       "value": ["unemployed", "preparing"], "seg_id": "0123456789ab",
                       "evidence": "부산 거주 미취업 청년"}],
            "code_rules": [{"id": "C-job", "field": "employment_status", "op": "in",
                            "value": ["unemployed", "preparing", "student"], "origin": "source_codes",
                            "from": ["jobCd"]}],
            "window": {"kind": "unknown"},
            "remaining_checks": [],
            // Every segment labelled: only a covered set may be complete.
            "coverage": {"agreement": "agreed", "labels":
                mazimi_core::condition_review::coverage::segments("youth_center", payload)
                    .unwrap()
                    .into_iter()
                    .map(|s| (s.seg_id, json!({"label": "non_condition"})))
                    .collect::<serde_json::Map<String, Value>>()},
            "reviewer": "curator:A+B@2026-10-15",
            "title": "annotation, ignored"
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base.to_string()
    }

    async fn stored(connection: &mut sqlx::PgConnection, ids: &[String]) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM program_condition_reviews WHERE source_id = ANY($1)",
        )
        .bind(ids)
        .fetch_one(connection)
        .await
        .unwrap()
    }

    /// Code overrides are validated against the programme's columns and
    /// counted by target and action; a dry run writes nothing.
    #[tokio::test]
    async fn imports_code_overrides_checked_against_the_programme() {
        let mut db = connection().await;
        let mut outer = db.begin().await.unwrap();
        let a = format!("t{}", uuid::Uuid::new_v4().simple());
        let mut pa = raw("청년도전지원사업 검증", "0057002");
        pa["addAplyQlfcCndCn"] = json!("군 경력자는 최고 만 39세로 한정");
        sqlx::query("INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active,raw_payload,min_age,max_age) VALUES('policy','youth_center',$1,'청년도전지원사업 검증','active',true,$2,18,34)")
            .bind(&a)
            .bind(&pa)
            .execute(&mut *outer)
            .await
            .unwrap();
        let with = |value: Value| {
            line(
                &a,
                &pa,
                json!({"status": "incomplete", "remaining_checks": ["other"],
                       "code_overrides": [{"target": "S-age", "action": "widen",
                           "replacement": {"field": "age", "op": "between", "value": value},
                           "check_kind": "military_service",
                           "evidence": "군 경력자는 최고 만 39세로 한정"}]}),
            )
        };
        let summary = import_on(&mut outer, &with(json!([18, 39])), "import:test", true)
            .await
            .unwrap();
        assert_eq!((summary.inserted, summary.applied), (1, false));
        assert_eq!(summary.overrides_by_target["S-age:widen"], 1);
        assert_eq!(stored(&mut outer, std::slice::from_ref(&a)).await, 0);

        let summary = import_on(&mut outer, &with(json!([20, 39])), "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.rejected_by_reason["override_not_wider"], 1);
        assert!(summary.overrides_by_target.is_empty());

        let summary = import_on(&mut outer, &with(json!([18, 39])), "import:test", false)
            .await
            .unwrap();
        assert!(summary.applied);
        let stored_overrides: Value = sqlx::query_scalar(
            "SELECT code_overrides FROM program_condition_reviews WHERE source_id = $1",
        )
        .bind(&a)
        .fetch_one(&mut *outer)
        .await
        .unwrap();
        assert_eq!(stored_overrides[0]["target"], "S-age");
        assert_eq!(stored_overrides[0]["replacement"]["value"], json!([18, 39]));
        outer.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn imports_all_or_nothing_and_rejects_stale_text_bad_rules_and_non_quotes() {
        let mut db = connection().await;
        let mut outer = db.begin().await.unwrap();
        let a = format!("t{}", uuid::Uuid::new_v4().simple());
        let b = format!("t{}", uuid::Uuid::new_v4().simple());
        let pa = raw("부산 청년 구직 지원 검증", "0057002");
        let pb = raw("부산 청년 면접 정장 검증", "0057001");
        programme(&mut outer, &a, &pa).await;
        programme(&mut outer, &b, &pb).await;
        let good = [
            line(&a, &pa, json!({})),
            line(
                &b,
                &pb,
                json!({"status": "incomplete", "remaining_checks": ["duplicate_benefit"]}),
            ),
        ]
        .join("\n");
        let bad_lines = [
            // text_changed: the curators read another version.
            line(&a, &pa, json!({"text_hash": "0".repeat(64)})),
            // invalid rule leaf: a field the rule engine does not have.
            line(
                &a,
                &pa,
                json!({"rules": [{"id": "R1", "field": "marital_status", "op": "in",
                                   "value": ["single"], "evidence": "부산 거주 미취업 청년"}]}),
            ),
            // evidence not a quote.
            line(
                &a,
                &pa,
                json!({"rules": [{"id": "R1", "field": "employment_status", "op": "eq",
                                   "value": "unemployed", "evidence": "구직 중인 부산 청년"}]}),
            ),
            // bad enums.
            line(&a, &pa, json!({"status": "verified"})),
            line(&a, &pa, json!({"window": {"kind": "sometimes"}})),
            line(
                &a,
                &pa,
                json!({"status": "incomplete", "remaining_checks": ["shoe_size"]}),
            ),
            line(&a, &pa, json!({"remaining_checks": ["assets"]})),
            line("missing-id", &pa, json!({})),
            "not json".to_owned(),
        ];
        for (i, bad) in bad_lines.iter().enumerate() {
            let text = format!("{good}\n{bad}");
            let summary = import_on(&mut outer, &text, "import:test", false)
                .await
                .unwrap();
            assert!(!summary.applied, "{i}");
            assert_eq!(summary.rejected, 1, "{i}: {summary:?}");
            assert_eq!(
                stored(&mut outer, &[a.clone(), b.clone()]).await,
                0,
                "{i}: nothing written on rejection"
            );
        }
        // Each rejection names its reason.
        let mut seen = Vec::new();
        for bad in &bad_lines {
            let summary = import_on(&mut outer, bad, "import:test", false)
                .await
                .unwrap();
            seen.extend(summary.rejected_by_reason.into_keys());
        }
        assert_eq!(
            seen,
            [
                "text_changed",
                "invalid_rule",
                "evidence_not_found",
                "unknown_status",
                "invalid_window",
                "unknown_check_kind",
                "inconsistent_status",
                "program_not_found",
                "invalid_json"
            ]
        );
        // A duplicate key in one file.
        let summary = import_on(
            &mut outer,
            &format!("{good}\n{}", line(&a, &pa, json!({}))),
            "import:test",
            false,
        )
        .await
        .unwrap();
        assert_eq!(summary.rejected_by_reason["duplicate"], 1);

        let summary = import_on(&mut outer, &good, "import:test", true)
            .await
            .unwrap();
        assert_eq!(
            (summary.inserted, summary.rejected, summary.applied),
            (2, 0, false)
        );
        assert_eq!(stored(&mut outer, &[a.clone(), b.clone()]).await, 0);

        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!((summary.inserted, summary.applied), (2, true));
        assert_eq!(summary.accepted_by_status["complete"], 1);
        let (rule, hash, coverage): (Value, String, Value) = sqlx::query_as(
            "SELECT rule_json, text_hash, coverage_json FROM program_condition_reviews WHERE source_id = $1",
        )
        .bind(&a)
        .fetch_one(&mut *outer)
        .await
        .unwrap();
        assert_eq!(rule["all"][0]["origin"], "source_codes");
        assert_eq!(rule["all"][1]["evidence"], "부산 거주 미취업 청년");
        assert_eq!(Some(hash), text_hash("youth_center", &pa));
        assert_eq!(coverage["agreement"], "agreed");
        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.unchanged, 2, "re-import is idempotent");
        let logged: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM program_condition_review_log WHERE source_id = ANY($1) AND actor = 'import:test'",
        )
        .bind(vec![a.clone(), b.clone()])
        .fetch_one(&mut *outer)
        .await
        .unwrap();
        assert_eq!(logged, 2);

        // The log is append-only in the database itself.
        for statement in [
            "UPDATE program_condition_review_log SET actor = 'x' WHERE source_id = $1",
            "DELETE FROM program_condition_review_log WHERE source_id = $1",
        ] {
            let mut savepoint = outer.begin().await.unwrap();
            let error = sqlx::query(statement)
                .bind(&a)
                .execute(&mut *savepoint)
                .await
                .expect_err(statement);
            assert!(error.to_string().contains("append-only"), "{error}");
            savepoint.rollback().await.unwrap();
        }

        // A text change after import makes a re-import of the old line stale.
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"aplyPrdSeCd\":\"0057003\"}' WHERE source_type='youth_center' AND source_id=$1")
            .bind(&a).execute(&mut *outer).await.unwrap();
        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.rejected_by_reason["text_changed"], 1);
        outer.rollback().await.unwrap();
    }

    /// A 온통청년 sync run finished at `at` that returned `seen` with `item`.
    async fn sync_run(
        pool: &PgPool,
        status: &str,
        at: DateTime<Utc>,
        seen: &[(&str, &str)],
    ) -> uuid::Uuid {
        let run: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO ingestion_runs(source_name,status,started_at,finished_at,run_kind)
             VALUES('youth_center',$1,$2 - interval '10 minutes',$2,'ingestion') RETURNING id",
        )
        .bind(status)
        .bind(at)
        .fetch_one(pool)
        .await
        .unwrap();
        for (source_id, item) in seen {
            sqlx::query("INSERT INTO ingestion_items(run_id,source_id,status) VALUES($1,$2,$3)")
                .bind(run)
                .bind(source_id)
                .bind(item)
                .execute(pool)
                .await
                .unwrap();
        }
        run
    }

    #[tokio::test]
    async fn confirmation_comes_from_a_fresh_successful_sync_without_network() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        // A timeline far ahead of every other test's runs (the database is
        // shared): these runs are the latest 온통청년 syncs.
        let now = DateTime::from_timestamp_micros(
            (Utc::now() + chrono::Duration::days(3650)).timestamp_micros(),
        )
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let base: u64 =
            90_000_000_000 + (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as u64 * 10;
        let ids: Vec<String> = (0..7).map(|i| (base + i).to_string()).collect();
        let [still, reopen, old, unseen, quarantined, stale, unresolved] =
            [0, 1, 2, 3, 4, 5, 6].map(|i| ids[i].clone());
        let mut lines = HashMap::new();
        for id in &ids {
            let payload = raw(&format!("청년 상시 지원 검증 {id}"), "0057002");
            programme(&mut conn, id, &payload).await;
            // The curators labelled window text they could not resolve.
            let extra = if id == &unresolved {
                let mut coverage: Value =
                    serde_json::from_str::<Value>(&line(id, &payload, json!({}))).unwrap()
                        ["coverage"]
                        .clone();
                coverage["labels"]["0123456789ab"] = json!({"label": "window", "evidence": "x"});
                json!({ "coverage": coverage })
            } else {
                json!({})
            };
            let text = line(id, &payload, extra);
            let summary = import_on(&mut conn, &text, "import:test", false)
                .await
                .unwrap();
            assert!(summary.applied, "{summary:?}");
            lines.insert(id.clone(), text);
        }
        // Closed by an earlier official re-check: seen 상시 again, it reopens.
        let hash = text_hash(
            "youth_center",
            &raw(&format!("청년 상시 지원 검증 {reopen}"), "0057002"),
        )
        .unwrap();
        assert!(store::confirm_window(
            &mut conn,
            "youth_center",
            &reopen,
            &hash,
            Confirmation::Closed,
            now - chrono::Duration::days(3),
            "test",
        )
        .await
        .unwrap());
        // Text changed after review: not a candidate.
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"plcySprtCn\":\"월 50만원\"}' WHERE source_type='youth_center' AND source_id=$1")
            .bind(&stale).execute(&mut *conn).await.unwrap();
        drop(conn);

        let seen_all: Vec<(&str, &str)> = [&still, &reopen, &stale, &unresolved]
            .iter()
            .map(|id| (id.as_str(), "unchanged"))
            .chain([(quarantined.as_str(), "quarantined")])
            .collect();
        let mut runs = vec![
            // Only a sync older than 36 h saw `old`.
            sync_run(
                &pool,
                "success",
                now - chrono::Duration::hours(40),
                &[(&old, "upserted")],
            )
            .await,
            sync_run(
                &pool,
                "success",
                now - chrono::Duration::hours(3),
                &seen_all,
            )
            .await,
            // The latest sync failed: degraded, nothing new confirmed.
            sync_run(&pool, "failed", now - chrono::Duration::hours(2), &[]).await,
        ];
        let config = ConfirmationConfig { max_programs: 1500 };
        let ours = |summary: &ConfirmationSummary| summary.candidates;
        let window = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Value, Option<DateTime<Utc>>)>(
                    "SELECT window_json, window_confirmed_at FROM program_condition_reviews WHERE source_type='youth_center' AND source_id=$1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let untouched = (
            json!({"kind": "unknown", "opens_at": null, "closes_at": null}),
            None,
        );
        let degraded = confirm_rolling_windows(&pool, &config, false, now)
            .await
            .unwrap();
        assert_eq!(
            degraded.degraded.as_deref(),
            Some("latest 온통청년 sync failed")
        );
        assert_eq!(degraded.confirmed_rolling, 0);
        assert!(ours(&degraded) >= 5);
        assert_eq!(window(still.clone()).await, untouched);

        // The hourly retry succeeds and also sees `still`.
        let retry_at = now - chrono::Duration::hours(1);
        runs.push(
            sync_run(
                &pool,
                "success_with_rejections",
                retry_at,
                &[(&still, "upserted")],
            )
            .await,
        );

        // Only this test's rows (the database is shared).
        let fresh_since = now - chrono::Duration::hours(SYNC_FRESHNESS_HOURS);
        let candidates: Vec<Candidate> = confirmation_candidates(&pool, 1500, fresh_since)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| ids.contains(&c.1))
            .collect();
        let listed: Vec<&str> = candidates.iter().map(|c| c.1.as_str()).collect();
        assert!(!listed.contains(&stale.as_str()), "{listed:?}");
        assert!(!listed.contains(&unresolved.as_str()), "{listed:?}");
        assert_eq!(listed.len(), 5, "{listed:?}");
        let sync = latest_sync(&pool).await.unwrap();
        assert_eq!(sync, Some(("success_with_rejections".into(), retry_at)));

        let dry = confirm_candidates(&pool, sync.clone(), candidates.clone(), true, now)
            .await
            .unwrap();
        assert_eq!(
            (dry.confirmed_rolling, dry.not_seen_recently),
            (2, 3),
            "{dry:?}"
        );
        assert_eq!(
            window(still.clone()).await,
            untouched,
            "a dry run writes nothing"
        );

        let summary = confirm_candidates(&pool, sync, candidates, false, now)
            .await
            .unwrap();
        assert_eq!(summary.degraded, None);
        assert_eq!(
            (
                summary.candidates,
                summary.confirmed_rolling,
                summary.not_seen_recently
            ),
            (5, 2, 3),
            "{summary:?}"
        );
        // Confirmed at the finish time of the latest sync that saw it.
        let (w, at) = window(still.clone()).await;
        assert_eq!(w["kind"], "rolling");
        assert_eq!(w["source"], "official_recheck");
        assert_eq!(w["reviewed"]["kind"], "unknown");
        assert_eq!(at, Some(retry_at));
        let (w, at) = window(reopen.clone()).await;
        assert_eq!(w["kind"], "rolling");
        assert_eq!(
            w["reviewed"]["kind"], "unknown",
            "the reviewed window is kept"
        );
        assert_eq!(at, Some(now - chrono::Duration::hours(3)));
        // Seen only long ago, never, or quarantined: unchanged.
        for id in [
            old.clone(),
            unseen.clone(),
            quarantined.clone(),
            stale.clone(),
            unresolved.clone(),
        ] {
            assert_eq!(window(id).await, untouched);
        }
        let actors: Vec<String> = sqlx::query_scalar(
            "SELECT actor FROM program_condition_review_log WHERE source_id = ANY($1) AND action = 'confirm_window' AND actor <> 'test'",
        )
        .bind(&ids)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actors, vec![CONFIRMATION_ACTOR; 2]);

        // Re-importing the reviewed line keeps the confirmation; a new
        // reviewed window drops it.
        let mut conn = pool.acquire().await.unwrap();
        let summary = import_on(&mut conn, &lines[&still], "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.unchanged, 1, "{summary:?}");
        let (kept, kept_at) = window(still.clone()).await;
        assert_eq!(
            (kept["kind"].as_str(), kept_at),
            (Some("rolling"), Some(retry_at))
        );
        let payload = raw(&format!("청년 상시 지원 검증 {still}"), "0057002");
        let changed = line(
            &still,
            &payload,
            json!({"window": {"kind": "event", "evidence": "부산 거주 미취업 청년"}}),
        );
        let summary = import_on(&mut conn, &changed, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.updated, 1, "{summary:?}");
        let (w, at) = window(still.clone()).await;
        assert_eq!((w["kind"].as_str(), at), (Some("event"), None));
        drop(conn);

        // The review log is append-only, so the test rows stay; runs,
        // programmes and reviews are removed.
        sqlx::query("DELETE FROM ingestion_runs WHERE id = ANY($1)")
            .bind(&runs)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM program_condition_reviews WHERE source_type='youth_center' AND source_id = ANY($1)")
            .bind(&ids).execute(&pool).await.unwrap();
        sqlx::query(
            "DELETE FROM programs WHERE source_type='youth_center' AND source_id = ANY($1)",
        )
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
    }

    #[test]
    fn a_failed_missing_or_stale_sync_degrades_the_job() {
        let now = Utc::now();
        let hours = |h: i64| now - chrono::Duration::hours(h);
        assert_eq!(
            sync_degraded(None, now).as_deref(),
            Some("no finished 온통청년 sync")
        );
        assert!(sync_degraded(Some(&("failed".into(), hours(1))), now).is_some());
        assert!(sync_degraded(Some(&("running".into(), hours(1))), now).is_some());
        assert!(sync_degraded(Some(&("success".into(), hours(37))), now).is_some());
        assert_eq!(
            sync_degraded(Some(&("success".into(), hours(35))), now),
            None
        );
        assert_eq!(
            sync_degraded(Some(&("success_with_rejections".into(), hours(1))), now),
            None
        );
    }

    /// The SQL text hash the candidate query filters on is the Rust
    /// `text_hash` the importer stores.
    #[tokio::test]
    async fn sql_text_hash_matches_the_stored_review_hash() {
        let mut db = connection().await;
        let mut payloads = vec![raw("청년 상시 지원 해시", "0057002"), json!({})];
        let mut odd = raw("청년\n\"따옴표\" 지원", "0057002");
        odd["sprtTrgtMinAge"] = Value::Null;
        odd.as_object_mut().unwrap().remove("zipCd");
        payloads.push(odd);
        for payload in payloads {
            let sql: Option<String> = sqlx::query_scalar(&format!(
                "SELECT {} FROM (SELECT 'youth_center'::text AS source_type, $1::jsonb AS raw_payload) p",
                review_text_hash_sql("p.")
            ))
            .bind(&payload)
            .fetch_one(&mut db)
            .await
            .unwrap();
            assert_eq!(sql, text_hash("youth_center", &payload), "{payload}");
        }
    }

    #[test]
    fn confirmation_is_off_unless_enabled() {
        // Read-only check of the default; the variable is not set in tests.
        if std::env::var("ROLLING_WINDOW_CONFIRMATION_ENABLED").is_err() {
            assert!(confirmation_config_from_env().unwrap().is_none());
        }
    }
}
