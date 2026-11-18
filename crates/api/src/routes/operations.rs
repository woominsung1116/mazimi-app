//! Admin-only operational incident and data-quality queues.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AdminUser;

type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn validate_note(note: Option<String>, required: bool) -> Result<Option<String>, ApiError> {
    let note = note.map(|value| value.trim().to_string());
    if required && note.as_deref().is_none_or(str::is_empty) {
        return Err(bad_request("note is required for this action"));
    }
    if note
        .as_deref()
        .is_some_and(|value| value.is_empty() || value.chars().count() > 1_000)
    {
        return Err(bad_request("note must contain 1 to 1000 characters"));
    }
    if note
        .as_deref()
        .is_some_and(|value| value.contains('<') || value.contains('>'))
    {
        return Err(bad_request("note must be plain text"));
    }
    Ok(note.filter(|value| !value.is_empty()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueQuery {
    pub status: Option<String>,
    pub severity: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramMaintenanceQuery {
    /// Maximum rows returned per queue. Counts always cover the full queue.
    pub limit: Option<i64>,
}

#[derive(Debug, FromRow, Serialize)]
pub struct ProgramLifecycleResponse {
    pub id: Uuid,
    pub title: String,
    pub provider_name: Option<String>,
    pub official_url: Option<String>,
    pub program_status: String,
    pub is_active: bool,
    pub effective_end_at: DateTime<Utc>,
    pub source_review_due_at: Option<DateTime<Utc>>,
    /// `lifecycle` when the program's own end date drives `effective_end_at`,
    /// `source_review` when the official-source re-verification deadline does.
    pub expiry_kind: String,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DuplicateProgramGroupResponse {
    pub match_type: String,
    pub match_key: String,
    pub item_count: i64,
    pub active_count: i64,
    pub program_ids: Vec<Uuid>,
    pub titles: Vec<String>,
    pub providers: Vec<String>,
}

#[derive(Debug, FromRow, Serialize)]
pub struct SourceReviewResponse {
    pub program_id: Uuid,
    pub title: String,
    pub provider_name: Option<String>,
    pub source_health: String,
    pub source_review_due_at: Option<DateTime<Utc>>,
    pub open_hold_count: i64,
}

/// GET /api/v1/admin/program-maintenance
///
/// Read-only operator queues for lifecycle, conservative exact-duplicate
/// candidates, and official-source reviews. Duplicate matches are deliberately
/// limited to canonical URL or exact normalized provider/title pairs; fuzzy
/// matching belongs in a separately reviewed enrichment workflow.
pub async fn get_program_maintenance(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<ProgramMaintenanceQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let lifecycle_end = "LEAST(benefit_valid_until, deadline_at, application_end_at)";
    // A published official benefit also disappears when its source
    // re-verification deadline passes, so the expiring window counts both.
    let expiring_end =
        "LEAST(benefit_valid_until, deadline_at, application_end_at, source_review_due_at)";
    let expiry_kind = format!(
        "CASE WHEN source_review_due_at IS NOT NULL
               AND source_review_due_at <= COALESCE({lifecycle_end}, source_review_due_at)
              THEN 'source_review' ELSE 'lifecycle' END"
    );

    let (expired_count, expiring_count) = sqlx::query_as::<_, (i64, i64)>(&format!(
        "SELECT
            count(*) FILTER (
                WHERE is_active = true AND ({lifecycle_end}) < now()
            ),
            count(*) FILTER (
                WHERE is_active = true
                  AND ({expiring_end}) >= now()
                  AND ({expiring_end}) < now() + interval '30 days'
            )
         FROM programs"
    ))
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let expired = sqlx::query_as::<_, ProgramLifecycleResponse>(&format!(
        "SELECT id, title, provider_name, official_url, program_status,
                is_active, ({lifecycle_end}) AS effective_end_at,
                source_review_due_at, 'lifecycle'::text AS expiry_kind
         FROM programs
         WHERE is_active = true AND ({lifecycle_end}) < now()
         ORDER BY ({lifecycle_end}) ASC, id
         LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let expiring = sqlx::query_as::<_, ProgramLifecycleResponse>(&format!(
        "SELECT id, title, provider_name, official_url, program_status,
                is_active, ({expiring_end}) AS effective_end_at,
                source_review_due_at, ({expiry_kind}) AS expiry_kind
         FROM programs
         WHERE is_active = true
           AND ({expiring_end}) >= now()
           AND ({expiring_end}) < now() + interval '30 days'
         ORDER BY ({expiring_end}) ASC, id
         LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let duplicate_groups = sqlx::query_as::<_, DuplicateProgramGroupResponse>(
        r#"
        WITH normalized AS (
            SELECT id, title, COALESCE(provider_name, '') AS provider_name,
                   is_active,
                   NULLIF(
                       regexp_replace(
                           regexp_replace(lower(btrim(COALESCE(official_url, ''))), '#.*$', ''),
                           '/+$', ''
                       ),
                       ''
                   ) AS canonical_url,
                   NULLIF(
                       regexp_replace(lower(btrim(COALESCE(provider_name, ''))), '\s+', ' ', 'g')
                       || '|' ||
                       regexp_replace(lower(btrim(title)), '\s+', ' ', 'g'),
                       '|'
                   ) AS provider_title
            FROM programs
        ), matches AS (
            SELECT 'official_url'::text AS match_type, canonical_url AS match_key,
                   count(*)::bigint AS item_count,
                   count(*) FILTER (WHERE is_active)::bigint AS active_count,
                   array_agg(id ORDER BY created_at_fallback, id) AS program_ids,
                   array_agg(title ORDER BY created_at_fallback, id) AS titles,
                   array_agg(provider_name ORDER BY created_at_fallback, id) AS providers
            FROM (
                SELECT normalized.*, id::text AS created_at_fallback
                FROM normalized WHERE canonical_url IS NOT NULL
            ) url_match
            GROUP BY canonical_url HAVING count(*) > 1
            UNION ALL
            SELECT 'provider_title'::text, provider_title,
                   count(*)::bigint,
                   count(*) FILTER (WHERE is_active)::bigint,
                   array_agg(id ORDER BY created_at_fallback, id),
                   array_agg(title ORDER BY created_at_fallback, id),
                   array_agg(provider_name ORDER BY created_at_fallback, id)
            FROM (
                SELECT normalized.*, id::text AS created_at_fallback
                FROM normalized WHERE provider_title IS NOT NULL
            ) title_match
            GROUP BY provider_title HAVING count(*) > 1
        )
        SELECT match_type, match_key, item_count, active_count,
               program_ids, titles, providers
        FROM matches
        ORDER BY active_count DESC, item_count DESC, match_type, match_key
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let duplicate_group_count = sqlx::query_scalar::<_, i64>(
        r#"
        WITH normalized AS (
            SELECT
                NULLIF(regexp_replace(regexp_replace(lower(btrim(COALESCE(official_url, ''))), '#.*$', ''), '/+$', ''), '') AS canonical_url,
                NULLIF(regexp_replace(lower(btrim(COALESCE(provider_name, ''))), '\s+', ' ', 'g') || '|' || regexp_replace(lower(btrim(title)), '\s+', ' ', 'g'), '|') AS provider_title
            FROM programs
        )
        SELECT
            (SELECT count(*) FROM (SELECT canonical_url FROM normalized WHERE canonical_url IS NOT NULL GROUP BY canonical_url HAVING count(*) > 1) groups_by_url)
          + (SELECT count(*) FROM (SELECT provider_title FROM normalized WHERE provider_title IS NOT NULL GROUP BY provider_title HAVING count(*) > 1) groups_by_title)
        "#,
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let source_review_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM programs program
         WHERE program.source_type IN ('corporate_official','university_official')
           AND (
               program.source_health <> 'current'
               OR program.source_review_due_at <= now()
               OR EXISTS (
                   SELECT 1 FROM program_source_holds hold
                   WHERE hold.program_id = program.id AND hold.resolved_at IS NULL
               )
           )",
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let source_reviews = sqlx::query_as::<_, SourceReviewResponse>(
        "SELECT program.id AS program_id, program.title, program.provider_name,
                program.source_health, program.source_review_due_at,
                (SELECT count(*) FROM program_source_holds hold
                 WHERE hold.program_id = program.id AND hold.resolved_at IS NULL) AS open_hold_count
         FROM programs program
         WHERE program.source_type IN ('corporate_official','university_official')
           AND (
               program.source_health <> 'current'
               OR program.source_review_due_at <= now()
               OR EXISTS (
                   SELECT 1 FROM program_source_holds hold
                   WHERE hold.program_id = program.id AND hold.resolved_at IS NULL
               )
           )
         ORDER BY open_hold_count DESC, program.source_review_due_at ASC NULLS FIRST, program.id
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let daily_jobs = sqlx::query_scalar::<_,Value>(
        "SELECT jsonb_build_object('name',job_name,'status',status,'last_attempt_at',last_attempt_at,'last_success_at',last_success_at,'details',details,'stale',last_success_at IS NULL OR last_success_at<now()-interval '26 hours') FROM worker_daily_jobs WHERE job_name NOT LIKE 'marker:%' ORDER BY job_name")
        .fetch_all(&pool).await.map_err(crate::errors::internal_error)?;
    Ok(Json(json!({
        "daily_jobs":daily_jobs,
        "expired": { "total": expired_count, "items": expired },
        "expiring": { "total": expiring_count, "items": expiring },
        "duplicates": { "total": duplicate_group_count, "items": duplicate_groups },
        "source_reviews": { "total": source_review_count, "items": source_reviews },
        "limit": limit
    })))
}

#[derive(Debug, FromRow, Serialize)]
pub struct OperationalEventResponse {
    pub id: Uuid,
    pub fingerprint: String,
    pub severity: String,
    pub component: String,
    pub event_type: String,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub message: String,
    pub details: Value,
    pub status: String,
    pub occurrence_count: i32,
    pub first_occurred_at: DateTime<Utc>,
    pub last_occurred_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_note: Option<String>,
    pub delivery_attempt_count: i32,
    pub delivered_at: Option<DateTime<Utc>>,
    pub last_delivery_error: Option<String>,
}

const OPERATIONAL_EVENT_RETURNING: &str =
    "id, fingerprint, severity, component, event_type, subject_type, subject_id,
     message, details, status, occurrence_count, first_occurred_at,
     last_occurred_at, acknowledged_at, resolved_at, resolution_note,
     delivery_attempt_count, delivered_at, last_delivery_error";

/// GET /api/v1/admin/operational-events
pub async fn list_operational_events(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    if query
        .status
        .as_deref()
        .is_some_and(|status| !matches!(status, "open" | "acknowledged" | "resolved"))
    {
        return Err(bad_request("Invalid operational event status"));
    }
    if query
        .severity
        .as_deref()
        .is_some_and(|severity| !matches!(severity, "info" | "warning" | "error" | "critical"))
    {
        return Err(bad_request("Invalid operational event severity"));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0).max(0);
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM operational_events
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR severity = $2)",
    )
    .bind(query.status.as_deref())
    .bind(query.severity.as_deref())
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let items = sqlx::query_as::<_, OperationalEventResponse>(&format!(
        "SELECT {OPERATIONAL_EVENT_RETURNING}
         FROM operational_events
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR severity = $2)
         ORDER BY
             CASE severity
                 WHEN 'critical' THEN 3 WHEN 'error' THEN 2
                 WHEN 'warning' THEN 1 ELSE 0
             END DESC,
             last_occurred_at DESC
         LIMIT $3 OFFSET $4"
    ))
    .bind(query.status.as_deref())
    .bind(query.severity.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Ok(Json(json!({
        "total": total,
        "limit": limit,
        "offset": offset,
        "items": items
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOperationalEventInput {
    /// acknowledge | resolve | reopen
    pub action: String,
    pub note: Option<String>,
}

/// PUT /api/v1/admin/operational-events/{id}
pub async fn review_operational_event(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ReviewOperationalEventInput>,
) -> Result<Json<OperationalEventResponse>, ApiError> {
    if !matches!(input.action.as_str(), "acknowledge" | "resolve" | "reopen") {
        return Err(bad_request(
            "action must be acknowledge, resolve, or reopen",
        ));
    }
    let note = validate_note(input.note, input.action == "resolve")?;
    let event = match input.action.as_str() {
        "acknowledge" => {
            let query = format!(
                "UPDATE operational_events
             SET status = 'acknowledged', acknowledged_at = now(),
                 acknowledged_by = $2, resolution_note = COALESCE($3, resolution_note),
                 updated_at = now()
             WHERE id = $1 AND status <> 'resolved'
             RETURNING {OPERATIONAL_EVENT_RETURNING}"
            );
            sqlx::query_as::<_, OperationalEventResponse>(&query)
                .bind(id)
                .bind(admin.0.id)
                .bind(note)
                .fetch_optional(&pool)
                .await
        }
        "resolve" => {
            let query = format!(
                "UPDATE operational_events
             SET status = 'resolved', resolved_at = now(), resolved_by = $2,
                 resolution_note = $3, delivery_claim_token = NULL,
                 delivery_claimed_until = NULL, updated_at = now()
             WHERE id = $1 AND status <> 'resolved'
             RETURNING {OPERATIONAL_EVENT_RETURNING}"
            );
            sqlx::query_as::<_, OperationalEventResponse>(&query)
                .bind(id)
                .bind(admin.0.id)
                .bind(note)
                .fetch_optional(&pool)
                .await
        }
        "reopen" => {
            let query = format!(
                "UPDATE operational_events
             SET status = 'open', acknowledged_at = NULL, acknowledged_by = NULL,
                 resolved_at = NULL, resolved_by = NULL, resolution_note = $2,
                 delivered_at = NULL, delivery_attempt_count = 0,
                 next_delivery_at = now(), delivery_claim_token = NULL,
                 delivery_claimed_until = NULL, last_delivery_error = NULL,
                 updated_at = now()
             WHERE id = $1 AND status = 'resolved'
             RETURNING {OPERATIONAL_EVENT_RETURNING}"
            );
            sqlx::query_as::<_, OperationalEventResponse>(&query)
                .bind(id)
                .bind(note)
                .fetch_optional(&pool)
                .await
        }
        _ => unreachable!(),
    }
    .map_err(crate::errors::internal_error)?;

    if let Some(event) = event {
        return Ok(Json(event));
    }
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM operational_events WHERE id = $1)",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Err(if exists {
        (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Operational event is not valid for this action" })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Operational event not found" })),
        )
    })
}

#[derive(Debug, FromRow, Serialize)]
pub struct DataQualityIssueResponse {
    pub id: Uuid,
    pub program_id: Uuid,
    pub program_title: String,
    pub issue_code: String,
    pub original_value: Value,
    pub status: String,
    pub resolution_note: Option<String>,
    pub first_detected_at: DateTime<Utc>,
    pub last_detected_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// GET /api/v1/admin/data-quality-issues
pub async fn list_data_quality_issues(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    if query
        .status
        .as_deref()
        .is_some_and(|status| !matches!(status, "open" | "resolved" | "ignored"))
    {
        return Err(bad_request("Invalid data quality issue status"));
    }
    if query.severity.is_some() {
        return Err(bad_request(
            "severity is not supported for data quality issues",
        ));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0).max(0);
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM program_data_quality_issues
         WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(query.status.as_deref())
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let items = sqlx::query_as::<_, DataQualityIssueResponse>(
        "SELECT issue.id, issue.program_id, program.title AS program_title,
                issue.issue_code, issue.original_value, issue.status,
                issue.resolution_note, issue.first_detected_at,
                issue.last_detected_at, issue.resolved_at
         FROM program_data_quality_issues issue
         JOIN programs program ON program.id = issue.program_id
         WHERE ($1::text IS NULL OR issue.status = $1)
         ORDER BY issue.last_detected_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(query.status.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Ok(Json(json!({
        "total": total,
        "limit": limit,
        "offset": offset,
        "items": items
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDataQualityIssueInput {
    /// recheck | ignore | reopen
    pub action: String,
    pub note: Option<String>,
}

/// PUT /api/v1/admin/data-quality-issues/{id}
pub async fn review_data_quality_issue(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ReviewDataQualityIssueInput>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(input.action.as_str(), "recheck" | "ignore" | "reopen") {
        return Err(bad_request("action must be recheck, ignore, or reopen"));
    }
    let note = validate_note(input.note, input.action == "ignore")?;
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    let issue = sqlx::query_as::<_, (String, bool, String)>(
        "SELECT issue.issue_code,
                CASE issue.issue_code
                    WHEN 'invalid_title' THEN
                        char_length(btrim(program.title)) NOT BETWEEN 1 AND 300
                    WHEN 'invalid_age_range' THEN
                        (program.min_age IS NOT NULL AND program.min_age NOT BETWEEN 0 AND 100)
                        OR (program.max_age IS NOT NULL AND program.max_age NOT BETWEEN 0 AND 100)
                        OR (program.min_age IS NOT NULL AND program.max_age IS NOT NULL
                            AND program.min_age > program.max_age)
                    WHEN 'negative_benefit_amount' THEN
                        (program.benefit_amount_monthly IS NOT NULL AND program.benefit_amount_monthly < 0)
                        OR (program.benefit_amount_semester IS NOT NULL AND program.benefit_amount_semester < 0)
                        OR (program.benefit_amount_once IS NOT NULL AND program.benefit_amount_once < 0)
                    WHEN 'invalid_date_range' THEN
                        program.application_start_at IS NOT NULL
                        AND program.application_end_at IS NOT NULL
                        AND program.application_end_at < program.application_start_at
                    WHEN 'unknown_region' THEN
                        program.regions IS NOT NULL AND NOT program.regions <@ ARRAY[
                            'seoul', 'busan', 'daegu', 'incheon', 'gwangju', 'daejeon',
                            'ulsan', 'sejong', 'gyeonggi', 'gangwon', 'chungbuk', 'chungnam',
                            'jeonbuk', 'jeonnam', 'gyeongbuk', 'gyeongnam', 'jeju'
                        ]::text[]
                    WHEN 'missing_official_url' THEN
                        program.is_active = true AND (
                            program.official_url IS NULL
                            OR btrim(program.official_url) !~ '^https://[^[:space:]]+$'
                        )
                    WHEN 'missing_brand_link' THEN
                        program.is_active = true AND NOT EXISTS (
                            SELECT 1 FROM program_brands AS link
                            WHERE link.program_id = program.id
                        )
                    WHEN 'missing_source_monitor' THEN
                        program.is_active = true AND NOT EXISTS (
                            SELECT 1 FROM program_source_monitors AS monitor
                            WHERE monitor.program_id = program.id
                              AND monitor.verification_state = 'current'
                        )
                    ELSE true
                END AS still_invalid,
                issue.status
         FROM program_data_quality_issues issue
         JOIN programs program ON program.id = issue.program_id
         WHERE issue.id = $1
         FOR UPDATE OF issue, program",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Data quality issue not found" })),
        )
    })?;

    let status = match input.action.as_str() {
        "recheck" if issue.1 => {
            return Err((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "The program still violates this data quality rule",
                    "issue_code": issue.0
                })),
            ));
        }
        "recheck" => "resolved",
        "ignore" => "ignored",
        "reopen" if issue.2 == "open" => {
            return Err((
                StatusCode::CONFLICT,
                Json(json!({ "error": "Data quality issue is already open" })),
            ));
        }
        "reopen" => "open",
        _ => unreachable!(),
    };
    sqlx::query(
        "UPDATE program_data_quality_issues
         SET status = $2, resolution_note = $3,
             resolved_at = CASE WHEN $2 = 'resolved' THEN now() ELSE NULL END,
             last_detected_at = CASE WHEN $2 = 'open' THEN now() ELSE last_detected_at END
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(note)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Json(json!({ "id": id, "status": status })))
}
