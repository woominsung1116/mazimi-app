//! Admin side of AI extraction (A-09): the review queue, per-program review,
//! reviewer decisions, the audit of auto-applied facts and the one-shot
//! revert. A fact changes public data only through
//! `mazimi_worker::ai_extraction::apply::apply`, the same transactional path
//! the worker uses for rule-verified facts; here the actor is the admin from
//! the JWT.
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use mazimi_worker::ai_extraction::{
    apply::{self, Actor, ApplyError, RevertScope, OPEN_STATUSES},
    facts::FactValue,
    job::current_source,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::collections::HashSet;
use uuid::Uuid;

pub use apply::Decision;
pub(crate) use apply::{rule_condition, same_condition};

const ALL_STATUSES: &[&str] = &[
    "pending",
    "flagged",
    "invalid",
    "approved",
    "rejected",
    "superseded",
    "reverted",
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict {
        code: &'static str,
        message: &'static str,
        details: Value,
    },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<ApplyError> for Error {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Invalid(message) => Self::Invalid(message),
            ApplyError::NotFound => Self::NotFound,
            ApplyError::Conflict {
                code,
                message,
                details,
            } => Self::Conflict {
                code,
                message,
                details,
            },
            ApplyError::Database(error) => Self::Database(error),
            ApplyError::Internal(error) => Self::Internal(error),
        }
    }
}

impl Error {
    pub fn response(self) -> (StatusCode, Json<Value>) {
        match self {
            Self::Invalid(message) => (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": message, "code": "invalid_review"})),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                Json(
                    json!({"error": "프로그램 또는 후보를 찾을 수 없습니다.", "code": "candidate_not_found"}),
                ),
            ),
            Self::Conflict {
                code,
                message,
                details,
            } => (
                StatusCode::CONFLICT,
                Json(json!({"error": message, "code": code, "details": details})),
            ),
            // The data-quality CHECK constraints on programs reject an
            // approved value that contradicts the rest of the row.
            Self::Database(sqlx::Error::Database(error))
                if error.code().as_deref() == Some("23514") =>
            {
                (
                    StatusCode::BAD_REQUEST,
                    Json(
                        json!({"error": "승인 값이 프로그램 데이터 품질 규칙에 맞지 않습니다.", "code": "data_quality_violation"}),
                    ),
                )
            }
            Self::Database(error) => crate::errors::internal_error(error),
            Self::Internal(error) => crate::errors::internal_error(error),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

fn plain_note(note: &str) -> Result<String> {
    let note = note.trim();
    let count = note.chars().count();
    if !(10..=1000).contains(&count) || note.contains('<') || note.contains('>') {
        return Err(Error::Invalid(
            "note must be 10-1000 characters of plain text".into(),
        ));
    }
    Ok(note.to_string())
}

// ── Queue ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct QueueItem {
    pub id: Uuid,
    pub program_id: Uuid,
    pub program_title: String,
    pub fact_type: String,
    pub fact_key: String,
    pub value: Value,
    pub evidence: Option<Value>,
    pub confidence: Option<String>,
    pub status: String,
    pub validation: Value,
    pub model: String,
    pub auto_check: Option<Value>,
    pub applied_by: Option<String>,
    pub auto_rule: Option<String>,
    pub auto_parser_version: Option<String>,
    pub auto_run_id: Option<Uuid>,
    pub audit_sample: bool,
    pub audit_reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// `pending` (default) is the exception queue: HIGH, evidence-valid facts
/// that were not rule-verified and auto-applied. `auto_applied` lists what
/// the worker applied, `audit` the random sample still awaiting a human.
fn queue_filter(filter: Option<&str>) -> Result<(String, Vec<String>)> {
    let auto = "c.status = 'approved' AND c.applied_by = 'auto_verified'";
    Ok(match filter.unwrap_or("pending") {
        "auto_applied" => (auto.to_string(), vec![]),
        "audit" => (
            format!("{auto} AND c.audit_sample AND c.audit_reviewed_at IS NULL"),
            vec![],
        ),
        "open" => (
            "c.status = ANY($1)".into(),
            OPEN_STATUSES.iter().map(|s| s.to_string()).collect(),
        ),
        status if ALL_STATUSES.contains(&status) => {
            ("c.status = ANY($1)".into(), vec![status.to_string()])
        }
        _ => return Err(Error::Invalid("unknown status filter".into())),
    })
}

pub async fn queue(pool: &PgPool, query: QueueQuery) -> Result<Value> {
    let (clause, statuses) = queue_filter(query.status.as_deref())?;
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let order = if statuses.is_empty() {
        "c.reviewed_at DESC, c.id"
    } else {
        "c.created_at, c.id"
    };
    let items = sqlx::query_as::<_, QueueItem>(&format!(
        "SELECT c.id, c.program_id, p.title AS program_title, c.fact_type, c.fact_key,
                c.value, c.evidence, c.confidence, c.status, c.validation, c.model,
                c.auto_check, c.applied_by, c.auto_rule, c.auto_parser_version, c.auto_run_id,
                c.audit_sample, c.audit_reviewed_at, c.reviewed_at, c.created_at
         FROM program_ai_extraction_candidates c JOIN programs p ON p.id = c.program_id
         WHERE {clause}
         ORDER BY {order} LIMIT $2"
    ))
    .bind(&statuses)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(json!({ "items": items }))
}

// ── Per-program review ───────────────────────────────────────────────────

#[derive(Debug, FromRow)]
struct CandidateRow {
    id: Uuid,
    cache_id: Uuid,
    source_hash: String,
    fact_type: String,
    fact_key: String,
    value: Value,
    evidence: Option<Value>,
    confidence: Option<String>,
    status: String,
    validation: Value,
    model: String,
    prompt_version: String,
    reviewer_id: Option<Uuid>,
    reviewed_at: Option<DateTime<Utc>>,
    review_note: Option<String>,
    applied_changes: Option<Value>,
    auto_check: Option<Value>,
    applied_by: Option<String>,
    auto_rule: Option<String>,
    auto_parser_version: Option<String>,
    auto_run_id: Option<Uuid>,
    audit_sample: bool,
    audit_reviewed_at: Option<DateTime<Utc>>,
    reverted_at: Option<DateTime<Utc>>,
    revert_note: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow, Serialize)]
struct SourceRow {
    id: Uuid,
    source_hash: String,
    source_text: String,
    provider: String,
    model: String,
    prompt_version: String,
    created_at: DateTime<Utc>,
}

/// Everything the review panel needs: sources (with the exact text the
/// offsets index into), candidates, and per-fact current-vs-proposed diffs.
pub async fn program_review(pool: &PgPool, program_id: Uuid) -> Result<Value> {
    let mut conn = pool.acquire().await?;
    let state = apply::load_state(&mut conn, program_id, false).await?;
    let current = current_source(&mut *conn, program_id)
        .await?
        .ok_or(Error::NotFound)?;
    let candidates = sqlx::query_as::<_, CandidateRow>(
        "SELECT id, cache_id, source_hash, fact_type, fact_key, value, evidence, confidence,
                status, validation, model, prompt_version, reviewer_id, reviewed_at,
                review_note, applied_changes, auto_check, applied_by, auto_rule,
                auto_parser_version, auto_run_id, audit_sample, audit_reviewed_at,
                reverted_at, revert_note, created_at
         FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND status <> 'superseded'
         ORDER BY (status IN ('pending', 'flagged')) DESC, created_at DESC, fact_key
         LIMIT 200",
    )
    .bind(program_id)
    .fetch_all(&mut *conn)
    .await?;
    let cache_ids: Vec<Uuid> = candidates
        .iter()
        .map(|c| c.cache_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let sources = sqlx::query_as::<_, SourceRow>(
        "SELECT id, source_hash, source_text, provider, model, prompt_version, created_at
         FROM program_ai_extraction_cache WHERE id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(&cache_ids)
    .fetch_all(&mut *conn)
    .await?;
    let items: Vec<Value> = candidates
        .into_iter()
        .map(|c| {
            let plan = serde_json::from_value::<FactValue>(c.value.clone())
                .ok()
                .filter(|_| OPEN_STATUSES.contains(&c.status.as_str()))
                .map(|value| apply::plan_fact(&value, &state));
            json!({
                "id": c.id, "cache_id": c.cache_id, "source_hash": c.source_hash,
                "stale": c.source_hash != current.hash,
                "fact_type": c.fact_type, "fact_key": c.fact_key, "value": c.value,
                "evidence": c.evidence, "confidence": c.confidence, "status": c.status,
                "validation": c.validation, "model": c.model, "prompt_version": c.prompt_version,
                "reviewer_id": c.reviewer_id, "reviewed_at": c.reviewed_at,
                "review_note": c.review_note, "applied_changes": c.applied_changes,
                "auto_check": c.auto_check, "applied_by": c.applied_by,
                "auto_rule": c.auto_rule, "auto_parser_version": c.auto_parser_version,
                "auto_run_id": c.auto_run_id, "audit_sample": c.audit_sample,
                "audit_reviewed_at": c.audit_reviewed_at, "reverted_at": c.reverted_at,
                "revert_note": c.revert_note,
                "created_at": c.created_at, "plan": plan
            })
        })
        .collect();
    let sources: Vec<Value> = sources
        .into_iter()
        .map(|s| {
            let stale = s.source_hash != current.hash;
            let mut value = serde_json::to_value(s).unwrap_or(Value::Null);
            value["stale"] = json!(stale);
            value
        })
        .collect();
    Ok(json!({
        "program": {"id": program_id, "title": state.program.title},
        "current_source_hash": current.hash,
        "sources": sources,
        "candidates": items,
    }))
}

// ── Review ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    pub decisions: Vec<Decision>,
    pub note: String,
}

fn validate_input(input: &ReviewInput) -> Result<String> {
    let note = plain_note(&input.note)?;
    if !(1..=50).contains(&input.decisions.len()) {
        return Err(Error::Invalid("decisions must contain 1-50 items".into()));
    }
    let unique: HashSet<Uuid> = input.decisions.iter().map(|d| d.candidate_id).collect();
    if unique.len() != input.decisions.len() {
        return Err(Error::Invalid("each candidate may appear once".into()));
    }
    Ok(note)
}

/// Apply reviewer decisions for one program atomically. Either every
/// decision is recorded and every approved fact applied, or nothing is.
pub async fn review(
    pool: &PgPool,
    reviewer_id: Uuid,
    program_id: Uuid,
    input: ReviewInput,
) -> Result<Value> {
    let note = validate_input(&input)?;
    let mut tx = pool.begin().await?;
    let result = apply::apply(
        &mut tx,
        program_id,
        &input.decisions,
        &Actor::Reviewer {
            id: reviewer_id,
            note: &note,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(%program_id, %reviewer_id, decisions = input.decisions.len(), "ai extraction review applied");
    Ok(result)
}

// ── Auto-applied facts: runs, audit, revert ──────────────────────────────

#[derive(Debug, Serialize, FromRow)]
pub struct AutoRun {
    pub id: Uuid,
    pub trigger: String,
    pub status: String,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub auto_verified: i32,
    pub auto_applied: i32,
    pub auto_held: i32,
    pub auto_cap_skips: i32,
    pub audit_sampled: i32,
    pub still_applied: i64,
    pub reverted: i64,
    pub audit_open: i64,
}

/// Recent runs that applied facts automatically, for the revert action.
pub async fn auto_runs(pool: &PgPool) -> Result<Value> {
    let runs = sqlx::query_as::<_, AutoRun>(
        "SELECT r.id, r.trigger, r.status, r.model, r.started_at, r.finished_at,
                r.auto_verified, r.auto_applied, r.auto_held, r.auto_cap_skips, r.audit_sampled,
                count(c.id) FILTER (WHERE c.status = 'approved') AS still_applied,
                count(c.id) FILTER (WHERE c.status = 'reverted') AS reverted,
                count(c.id) FILTER (WHERE c.status = 'approved' AND c.audit_sample
                                     AND c.audit_reviewed_at IS NULL) AS audit_open
         FROM program_ai_extraction_runs r
         LEFT JOIN program_ai_extraction_candidates c
                ON c.auto_run_id = r.id AND c.applied_by = 'auto_verified'
         WHERE r.auto_applied > 0
         GROUP BY r.id
         ORDER BY r.started_at DESC
         LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    Ok(json!({ "runs": runs }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditInput {
    pub note: String,
}

/// A human confirms an audit-sampled automatic fact. Undoing one uses
/// [`revert`] with its `candidate_id`.
pub async fn confirm_audit(
    pool: &PgPool,
    reviewer_id: Uuid,
    candidate_id: Uuid,
    input: AuditInput,
) -> Result<Value> {
    let note = plain_note(&input.note)?;
    let updated = sqlx::query(
        "UPDATE program_ai_extraction_candidates
         SET audit_reviewed_at = now(), audit_reviewer_id = $2, audit_note = $3, updated_at = now()
         WHERE id = $1 AND status = 'approved' AND applied_by = 'auto_verified'
           AND audit_sample AND audit_reviewed_at IS NULL",
    )
    .bind(candidate_id)
    .bind(reviewer_id)
    .bind(&note)
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(Error::Conflict {
            code: "not_an_open_audit_sample",
            message: "감사 대기 중인 자동 반영 사실이 아닙니다.",
            details: json!({"candidate_id": candidate_id}),
        });
    }
    Ok(json!({"candidate_id": candidate_id, "audit": "confirmed"}))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevertInput {
    #[serde(default)]
    pub run_id: Option<Uuid>,
    #[serde(default)]
    pub parser_version: Option<String>,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub candidate_id: Option<Uuid>,
    pub note: String,
    #[serde(default)]
    pub dry_run: bool,
}

fn identifier(value: &str, what: &str) -> Result<String> {
    let ok = (1..=100).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(value.to_string())
    } else {
        Err(Error::Invalid(format!(
            "{what} must be 1-100 of [A-Za-z0-9_.-]"
        )))
    }
}

fn revert_scope(input: &RevertInput) -> Result<RevertScope> {
    let selectors = [
        input.run_id.is_some(),
        input.parser_version.is_some(),
        input.candidate_id.is_some(),
    ];
    if selectors.iter().filter(|s| **s).count() != 1 {
        return Err(Error::Invalid(
            "give exactly one of run_id, parser_version or candidate_id".into(),
        ));
    }
    if input.rule.is_some() && input.parser_version.is_none() {
        return Err(Error::Invalid("rule narrows parser_version only".into()));
    }
    Ok(
        match (&input.run_id, &input.parser_version, &input.candidate_id) {
            (Some(run), _, _) => RevertScope::Run(*run),
            (_, Some(version), _) => RevertScope::ParserVersion {
                version: identifier(version, "parser_version")?,
                rule: input
                    .rule
                    .as_deref()
                    .map(|r| identifier(r, "rule"))
                    .transpose()?,
            },
            (_, _, Some(candidate)) => RevertScope::Candidate(*candidate),
            _ => unreachable!("exactly one selector checked above"),
        },
    )
}

/// Undo auto-applied facts by run, parser version (optionally one rule) or
/// candidate. `dry_run` only counts what would be reverted.
pub async fn revert(pool: &PgPool, admin_id: Uuid, input: RevertInput) -> Result<Value> {
    let note = plain_note(&input.note)?;
    let scope = revert_scope(&input)?;
    let report = apply::revert(pool, &scope, admin_id, &note, input.dry_run).await?;
    Ok(serde_json::to_value(report).map_err(anyhow::Error::from)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apply::DecisionKind;

    #[test]
    fn review_input_requires_a_note_and_unique_decisions() {
        let id = Uuid::new_v4();
        let decision = |id| Decision {
            candidate_id: id,
            decision: DecisionKind::Approve,
            confirm_overwrite: false,
        };
        let short = ReviewInput {
            decisions: vec![decision(id)],
            note: "짧음".into(),
        };
        assert!(validate_input(&short).is_err());
        let duplicate = ReviewInput {
            decisions: vec![decision(id), decision(id)],
            note: "원문 공고와 대조해 확인했습니다".into(),
        };
        assert!(validate_input(&duplicate).is_err());
        let empty = ReviewInput {
            decisions: vec![],
            note: "원문 공고와 대조해 확인했습니다".into(),
        };
        assert!(validate_input(&empty).is_err());
        assert!(serde_json::from_value::<ReviewInput>(
            json!({"decisions": [], "note": "x", "user_id": id})
        )
        .is_err());
    }

    #[test]
    fn queue_filters_default_to_exceptions_and_know_auto_views() {
        assert_eq!(queue_filter(None).unwrap().1, vec!["pending".to_string()]);
        assert!(queue_filter(Some("auto_applied"))
            .unwrap()
            .0
            .contains("auto_verified"));
        assert!(queue_filter(Some("audit"))
            .unwrap()
            .0
            .contains("audit_sample"));
        assert_eq!(queue_filter(Some("reverted")).unwrap().1, vec!["reverted"]);
        assert!(queue_filter(Some("bogus")).is_err());
    }

    #[test]
    fn revert_needs_exactly_one_selector_and_safe_identifiers() {
        let input = |run: bool, version: Option<&str>, rule: Option<&str>| RevertInput {
            run_id: run.then(Uuid::new_v4),
            parser_version: version.map(str::to_string),
            rule: rule.map(str::to_string),
            candidate_id: None,
            note: "자동 반영 오류를 되돌립니다.".into(),
            dry_run: false,
        };
        assert!(matches!(
            revert_scope(&input(true, None, None)),
            Ok(RevertScope::Run(_))
        ));
        assert!(revert_scope(&input(true, Some("crosscheck-v1"), None)).is_err());
        assert!(revert_scope(&input(false, None, None)).is_err());
        assert!(revert_scope(&input(false, None, Some("age_range"))).is_err());
        assert_eq!(
            revert_scope(&input(false, Some("crosscheck-v1"), Some("age_range"))).unwrap(),
            RevertScope::ParserVersion {
                version: "crosscheck-v1".into(),
                rule: Some("age_range".into())
            }
        );
        assert!(revert_scope(&input(false, Some("v1'; DROP"), None)).is_err());
        assert!(serde_json::from_value::<RevertInput>(
            json!({"run_id": Uuid::nil(), "note": "x", "admin_id": Uuid::nil()})
        )
        .is_err());
    }
}
