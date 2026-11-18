//! The one transactional path that turns candidate facts into public data.
//! An admin approval (API) and an auto-verified fact (worker) both go
//! through [`apply`]; only the [`Actor`] and its provenance differ. Every
//! applied fact is re-validated against the program's current source inside
//! the caller's transaction.
//!
//! Where applied facts go:
//! - age → `programs.min_age/max_age` (+ `program_ai_field_overrides` so the
//!   next sync of the same upstream payload keeps it)
//! - dated window → `programs.application_start_at/end_at` (00:00 / 23:59:59
//!   KST via the pipeline helpers, + override)
//! - region, enrollment, employment, income decile / 학자금 지원구간 →
//!   a new `eligibility_rules` version with the condition in both
//!   `hard_filter_json` (excludes only an explicit contradiction; an unknown
//!   profile stays visible) and `rule_json` (explanations)
//! - documents → `program_documents` (`source = 'ai_extraction_approved'`, or
//!   `'ai_extraction_auto'` for an auto-verified fact)
//! - 상시 / 예산 소진 windows and 기준 중위소득 % have no public column or
//!   profile field: applying records the fact and changes nothing else.
//!
//! A value that already exists and differs is never overwritten without a
//! reviewer's `confirm_overwrite`; an automatic actor can never confirm.
use super::crosscheck::{cross_check, PARSER_VERSION};
use super::facts::{document_key, Evidence, FactValue, IncomeCriterion, WindowKind};
use super::job::current_source;
use super::source::ExtractionSource;
use super::validate::validate_fact;
use crate::pipeline::{kst_end_of_day, kst_start_of_day};
use chrono::{DateTime, Utc};
use mazimi_core::models::EligibilityRule;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection};
use std::collections::HashSet;
use uuid::Uuid;

pub const OPEN_STATUSES: &[&str] = &["pending", "flagged"];

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("conflict: {code}")]
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

pub type Result<T> = std::result::Result<T, ApplyError>;

pub fn conflict(code: &'static str, message: &'static str, details: Value) -> ApplyError {
    ApplyError::Conflict {
        code,
        message,
        details,
    }
}

/// Provenance of an automatic application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoProvenance {
    pub run_id: Uuid,
    /// The cross-check rule that read the same value (for example
    /// `age_range`).
    pub rule: String,
    pub parser_version: String,
    /// Flagged for post-hoc human review in the admin audit queue.
    pub audit_sample: bool,
}

/// Who applies a fact.
#[derive(Debug, Clone, Copy)]
pub enum Actor<'a> {
    /// An admin; the id comes from the verified JWT.
    Reviewer { id: Uuid, note: &'a str },
    /// The worker, for a fact a rule parser independently verified.
    Auto(&'a AutoProvenance),
}

impl Actor<'_> {
    fn reviewer_id(&self) -> Option<Uuid> {
        match self {
            Self::Reviewer { id, .. } => Some(*id),
            Self::Auto(_) => None,
        }
    }
    fn note(&self) -> String {
        match self {
            Self::Reviewer { note, .. } => note.to_string(),
            Self::Auto(p) => format!(
                "자동 검증 반영: 규칙 {} ({}), 실행 {}",
                p.rule, p.parser_version, p.run_id
            ),
        }
    }
    fn applied_by(&self) -> &'static str {
        match self {
            Self::Reviewer { .. } => "reviewer",
            Self::Auto(_) => "auto_verified",
        }
    }
    fn document_source(&self) -> &'static str {
        match self {
            Self::Reviewer { .. } => "ai_extraction_approved",
            Self::Auto(_) => "ai_extraction_auto",
        }
    }
    fn rule_created_by(&self) -> &'static str {
        match self {
            Self::Reviewer { .. } => "ai_extraction",
            Self::Auto(_) => "ai_extraction_auto",
        }
    }
    fn auto(&self) -> Option<&AutoProvenance> {
        match self {
            Self::Reviewer { .. } => None,
            Self::Auto(p) => Some(p),
        }
    }
}

// ── Current public state and per-fact plans ──────────────────────────────

#[derive(Debug, Clone, FromRow)]
pub struct ProgramState {
    pub title: String,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub application_start_at: Option<DateTime<Utc>>,
    pub application_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct PublicState {
    pub program: ProgramState,
    pub rule: Option<EligibilityRule>,
    /// `(document_key, name)` of existing checklist rows.
    pub documents: Vec<(String, String)>,
}

pub async fn load_state(
    conn: &mut PgConnection,
    program_id: Uuid,
    lock: bool,
) -> Result<PublicState> {
    let program = sqlx::query_as::<_, ProgramState>(&format!(
        "SELECT title, min_age, max_age, application_start_at, application_end_at
         FROM programs WHERE id = $1 {}",
        if lock { "FOR UPDATE" } else { "" }
    ))
    .bind(program_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ApplyError::NotFound)?;
    let rule = sqlx::query_as::<_, EligibilityRule>(
        "SELECT * FROM eligibility_rules WHERE program_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(program_id)
    .fetch_optional(&mut *conn)
    .await?;
    let documents = sqlx::query_scalar::<_, String>(
        "SELECT document_name FROM program_documents WHERE program_id = $1 ORDER BY sort_order",
    )
    .bind(program_id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|name| (document_key(&name), name))
    .collect();
    Ok(PublicState {
        program,
        rule,
        documents,
    })
}

/// One proposed change: where, what is there now, what applying writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Change {
    pub target: String,
    pub current: Value,
    pub proposed: Value,
    /// An existing, different value: applying needs `confirm_overwrite`.
    pub overwrites: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub notes: Vec<String>,
    /// Problems no confirmation can resolve (for example a nested rule).
    pub blockers: Vec<String>,
}

fn column_change<T: Serialize + PartialEq>(
    plan: &mut Plan,
    target: &str,
    current: Option<T>,
    proposed: T,
) {
    if current.as_ref() == Some(&proposed) {
        plan.notes
            .push(format!("{target}: already {}", json!(proposed)));
        return;
    }
    plan.changes.push(Change {
        target: target.to_string(),
        overwrites: current.is_some(),
        current: json!(current),
        proposed: json!(proposed),
    });
}

/// The rule condition an applied fact compiles to, if any.
pub fn rule_condition(value: &FactValue) -> Option<Value> {
    match value {
        FactValue::Region { codes } => {
            Some(json!({"field": "region_code", "op": "in", "value": codes, "weight": 1.0}))
        }
        FactValue::Enrollment { statuses } => Some(
            json!({"field": "enrollment_status", "op": "in", "value": statuses, "weight": 1.0}),
        ),
        FactValue::Employment { statuses } => Some(
            json!({"field": "employment_status", "op": "in", "value": statuses, "weight": 1.0}),
        ),
        FactValue::Income {
            criterion,
            comparator,
            value,
        } => {
            let field = match criterion {
                IncomeCriterion::IncomeDecile => "income_bracket",
                IncomeCriterion::KosafBracket => "kosaf_support_bracket",
                IncomeCriterion::MedianIncomePercent => return None,
            };
            Some(json!({"field": field, "op": comparator.as_str(), "value": value, "weight": 1.0}))
        }
        _ => None,
    }
}

fn same_field(a: &str, b: &str) -> bool {
    let canonical = |f: &str| if f == "region" { "region_code" } else { f }.to_string();
    canonical(a) == canonical(b)
}

pub fn same_condition(a: &Value, b: &Value) -> bool {
    a.get("field")
        .and_then(Value::as_str)
        .zip(b.get("field").and_then(Value::as_str))
        .is_some_and(|(x, y)| same_field(x, y))
        && a.get("op") == b.get("op")
        && a.get("value") == b.get("value")
}

/// Conditions that constrain the same thing: same field and the same bound
/// direction. An upper bound (`lte`/`lt`) and a lower bound (`gte`/`gt`) on
/// one field coexist; `between` overlaps both.
fn same_slot(a: &Value, b: &Value) -> bool {
    let class = |c: &Value| match c.get("op").and_then(Value::as_str) {
        Some("lte" | "lt") => "upper",
        Some("gte" | "gt") => "lower",
        Some("between") => "range",
        _ => "exact",
    };
    let field = |c: &Value| c.get("field").and_then(Value::as_str).map(str::to_string);
    let (ca, cb) = (class(a), class(b));
    field(a)
        .zip(field(b))
        .is_some_and(|(x, y)| same_field(&x, &y))
        && (ca == cb || ca == "range" || cb == "range")
}

/// Top-level children of a rule tree: the members of a root `all`, or the
/// root itself.
fn top_level(tree: &Value) -> Vec<Value> {
    match tree.get("all").and_then(Value::as_array) {
        Some(children) => children.clone(),
        None => vec![tree.clone()],
    }
}

/// `(top_level, condition)` for every condition on `field`.
fn conditions_on(tree: &Value, field: &str) -> Vec<(bool, Value)> {
    fn walk(node: &Value, field: &str, top: bool, out: &mut Vec<(bool, Value)>) {
        if let Some(children) = node.get("all").and_then(Value::as_array) {
            for child in children {
                walk(child, field, top, out);
            }
        } else if let Some(children) = node.get("any").and_then(Value::as_array) {
            for child in children {
                walk(child, field, false, out);
            }
        } else if node
            .get("field")
            .and_then(Value::as_str)
            .is_some_and(|f| same_field(f, field))
        {
            out.push((top, node.clone()));
        }
    }
    let mut out = Vec::new();
    // Only the root `all` counts as top level; a nested `all` inside `any`
    // is reached with top = false.
    match tree.get("all").and_then(Value::as_array) {
        Some(children) => {
            for child in children {
                if child.get("field").is_some() {
                    walk(child, field, true, &mut out);
                } else {
                    walk(child, field, false, &mut out);
                }
            }
        }
        None => walk(tree, field, tree.get("field").is_some(), &mut out),
    }
    out
}

fn rule_tree(rule: Option<&EligibilityRule>, hard: bool) -> Option<Value> {
    let rule = rule?;
    if hard {
        rule.hard_filter_json.clone()
    } else {
        Some(rule.rule_json.clone())
    }
}

pub fn plan_fact(value: &FactValue, state: &PublicState) -> Plan {
    let mut plan = Plan::default();
    let program = &state.program;
    match value {
        FactValue::Age {
            min_age, max_age, ..
        } => {
            if let Some(min) = min_age {
                column_change(&mut plan, "programs.min_age", program.min_age, *min);
            }
            if let Some(max) = max_age {
                column_change(&mut plan, "programs.max_age", program.max_age, *max);
            }
        }
        FactValue::ApplicationWindow {
            kind,
            start_date,
            end_date,
        } => {
            if let Some(start) = start_date {
                column_change(
                    &mut plan,
                    "programs.application_start_at",
                    program.application_start_at,
                    kst_start_of_day(*start),
                );
            }
            if let Some(end) = end_date {
                column_change(
                    &mut plan,
                    "programs.application_end_at",
                    program.application_end_at,
                    kst_end_of_day(*end),
                );
            }
            match kind {
                WindowKind::AlwaysOpen => plan.notes.push(
                    "상시 모집: recorded only; program_status is managed by the collector".into(),
                ),
                WindowKind::UntilBudgetExhausted => {
                    plan.notes.push("예산 소진 시 마감: recorded only".into())
                }
                WindowKind::Dated => {}
            }
        }
        FactValue::Document {
            name,
            required,
            notes,
        } => {
            let key = document_key(name);
            if let Some((_, existing)) = state.documents.iter().find(|(k, _)| *k == key) {
                plan.notes
                    .push(format!("program_documents: '{existing}' already listed"));
            } else {
                plan.changes.push(Change {
                    target: "program_documents".into(),
                    current: Value::Null,
                    proposed: json!({"document_name": name, "is_required": required, "description": notes}),
                    overwrites: false,
                });
            }
        }
        FactValue::Income {
            criterion: IncomeCriterion::MedianIncomePercent,
            ..
        } => plan.notes.push(
            "기준 중위소득 %: no profile field to compare; recorded only, no rule is compiled"
                .into(),
        ),
        other => {
            let Some(condition) = rule_condition(other) else {
                return plan;
            };
            let field = condition["field"].as_str().unwrap_or_default().to_string();
            for (hard, target) in [
                (true, "eligibility_rules.hard_filter_json"),
                (false, "eligibility_rules.rule_json"),
            ] {
                let existing: Vec<(bool, Value)> = rule_tree(state.rule.as_ref(), hard)
                    .map(|tree| conditions_on(&tree, &field))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, c)| same_slot(c, &condition))
                    .collect();
                if existing.iter().any(|(_, c)| same_condition(c, &condition)) {
                    plan.notes
                        .push(format!("{target}: condition already present"));
                    continue;
                }
                if existing.iter().any(|(top, _)| !top) {
                    plan.blockers.push(format!(
                        "{target}: '{field}' appears inside an any-branch; edit the rule manually"
                    ));
                    continue;
                }
                let current: Vec<Value> = existing.into_iter().map(|(_, c)| c).collect();
                plan.changes.push(Change {
                    target: target.to_string(),
                    overwrites: !current.is_empty(),
                    current: if current.is_empty() {
                        Value::Null
                    } else {
                        json!(current)
                    },
                    proposed: condition.clone(),
                });
            }
        }
    }
    plan
}

/// Rebuild a rule tree with `conditions` added at the top level; existing
/// top-level conditions in the same slot (field + bound direction) are
/// replaced, others on the same field (the opposite bound) are kept.
fn merge_tree(tree: Option<Value>, conditions: &[Value]) -> Value {
    let mut children = tree.as_ref().map(top_level).unwrap_or_default();
    for condition in conditions {
        children.retain(|child| child.get("field").is_none() || !same_slot(child, condition));
    }
    children.extend(conditions.iter().cloned());
    json!({ "all": children })
}

// ── Apply ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub candidate_id: Uuid,
    pub decision: DecisionKind,
    #[serde(default)]
    pub confirm_overwrite: bool,
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    id: Uuid,
    source_hash: String,
    fact_key: String,
    value: Value,
    evidence: Option<Value>,
    confidence: Option<String>,
    status: String,
    evidence_quote: Option<String>,
}

/// Re-check an automatic application inside the transaction, on the locked
/// row: still the default-queue state, HIGH, the same rule agreeing under the
/// current parser version, and no human rejected or reverted this fact for
/// the same source.
async fn verify_auto(
    conn: &mut PgConnection,
    program_id: Uuid,
    row: &CandidateRow,
    value: &FactValue,
    provenance: &AutoProvenance,
) -> Result<()> {
    let refuse = |reason: &str| {
        conflict(
            "auto_verification_failed",
            "자동 검증 조건을 다시 통과하지 못했습니다.",
            json!({"candidate_id": row.id, "reason": reason}),
        )
    };
    if row.status != "pending" {
        return Err(refuse("not in the default queue state"));
    }
    if row.confidence.as_deref() != Some("HIGH") {
        return Err(refuse("confidence is not HIGH"));
    }
    let check = cross_check(value, row.evidence_quote.as_deref().unwrap_or_default());
    if !check.agrees()
        || check.rule.as_deref() != Some(provenance.rule.as_str())
        || provenance.parser_version != PARSER_VERSION
    {
        return Err(refuse("the rule parser no longer reads the same value"));
    }
    // Same source, or the same value under any source: a text change (even
    // raw_payload noise) must not let a rejected or reverted value back in.
    let human_said_no: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM program_ai_extraction_candidates
                        WHERE program_id = $1 AND fact_key = $2 AND id <> $4
                          AND status IN ('rejected', 'reverted')
                          AND (source_hash = $3 OR value = $5))",
    )
    .bind(program_id)
    .bind(&row.fact_key)
    .bind(&row.source_hash)
    .bind(row.id)
    .bind(&row.value)
    .fetch_one(&mut *conn)
    .await?;
    if human_said_no {
        return Err(conflict(
            "human_decision_exists",
            "같은 사실을 사람이 거절하거나 되돌렸습니다.",
            json!({"candidate_id": row.id}),
        ));
    }
    Ok(())
}

/// Apply decisions for one program inside the caller's transaction. Either
/// every decision is recorded and every approved fact applied, or an error
/// is returned and the caller must roll back.
pub async fn apply(
    conn: &mut PgConnection,
    program_id: Uuid,
    decisions: &[Decision],
    actor: &Actor<'_>,
) -> Result<Value> {
    let mut state = load_state(conn, program_id, true).await?;
    let source: ExtractionSource = current_source(&mut *conn, program_id)
        .await?
        .ok_or(ApplyError::NotFound)?;
    let ids: Vec<Uuid> = decisions.iter().map(|d| d.candidate_id).collect();
    let rows = sqlx::query_as::<_, CandidateRow>(
        "SELECT id, source_hash, fact_key, value, evidence, confidence, status,
                evidence->>'quote' AS evidence_quote
         FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND id = ANY($2) FOR UPDATE",
    )
    .bind(program_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() != ids.len() {
        return Err(ApplyError::NotFound);
    }
    if let Some(row) = rows
        .iter()
        .find(|r| !OPEN_STATUSES.contains(&r.status.as_str()))
    {
        return Err(conflict(
            "candidate_not_open",
            "이미 검토했거나 승인할 수 없는 후보입니다.",
            json!({"candidate_id": row.id, "status": row.status}),
        ));
    }
    let reviewer_id = actor.reviewer_id();
    let note = actor.note();
    let auto = actor.auto();

    let mut approved_keys = HashSet::new();
    let mut rule_conditions: Vec<(bool, Value)> = Vec::new();
    let mut rule_candidates: Vec<Uuid> = Vec::new();
    // The candidate that changed each bound pair; both bounds of a changed
    // pair are pinned below so a re-normalized partner cannot cross them.
    let mut age_candidate: Option<Uuid> = None;
    let mut window_candidate: Option<Uuid> = None;
    let mut changed_fields: HashSet<String> = HashSet::new();
    let mut conflicts = Vec::new();
    let mut results = Vec::new();
    for decision in decisions {
        let row = rows
            .iter()
            .find(|r| r.id == decision.candidate_id)
            .expect("row loaded above");
        if decision.decision == DecisionKind::Reject {
            if auto.is_some() {
                return Err(ApplyError::Invalid(
                    "an automatic actor never rejects".into(),
                ));
            }
            sqlx::query(
                "UPDATE program_ai_extraction_candidates
                 SET status = 'rejected', reviewer_id = $2, reviewed_at = now(),
                     review_note = $3, updated_at = now()
                 WHERE id = $1",
            )
            .bind(row.id)
            .bind(reviewer_id)
            .bind(&note)
            .execute(&mut *conn)
            .await?;
            results.push(json!({"candidate_id": row.id, "decision": "reject"}));
            continue;
        }
        if !approved_keys.insert(row.fact_key.clone()) {
            return Err(ApplyError::Invalid(format!(
                "two approvals for the same fact '{}'",
                row.fact_key
            )));
        }
        if row.source_hash != source.hash {
            return Err(conflict(
                "source_changed",
                "공고 원문이 바뀌었습니다. 새 추출 결과로 다시 검토해 주세요.",
                json!({"candidate_id": row.id, "candidate_source_hash": row.source_hash, "current_source_hash": source.hash}),
            ));
        }
        let value: FactValue = serde_json::from_value(row.value.clone())
            .map_err(|e| anyhow::anyhow!("stored fact does not decode: {e}"))?;
        let evidence: Evidence = row
            .evidence
            .clone()
            .and_then(|e| serde_json::from_value(e).ok())
            .ok_or_else(|| anyhow::anyhow!("stored evidence does not decode"))?;
        let validation = validate_fact(&value, &evidence, &source);
        if !validation.errors.is_empty() {
            return Err(conflict(
                "evidence_invalid",
                "근거 검증을 다시 통과하지 못했습니다.",
                json!({"candidate_id": row.id, "errors": validation.errors}),
            ));
        }
        if let Some(provenance) = auto {
            if !validation.flags.is_empty() {
                return Err(conflict(
                    "auto_verification_failed",
                    "자동 검증 조건을 다시 통과하지 못했습니다.",
                    json!({"candidate_id": row.id, "reason": "validator warnings"}),
                ));
            }
            verify_auto(conn, program_id, row, &value, provenance).await?;
            // An end date already past is most likely an earlier round still
            // printed in the notice; filled in, it would close the programme
            // that night. A human decides.
            if let FactValue::ApplicationWindow {
                end_date: Some(end),
                ..
            } = &value
            {
                if kst_end_of_day(*end) <= Utc::now() {
                    return Err(conflict(
                        "window_end_passed",
                        "제안된 신청 마감일이 이미 지났습니다. 지난 회차인지 확인해 주세요.",
                        json!({"candidate_id": row.id, "end_date": end}),
                    ));
                }
            }
        }
        let plan = plan_fact(&value, &state);
        if !plan.blockers.is_empty() {
            return Err(conflict(
                "manual_edit_required",
                "규칙 구조상 자동 반영할 수 없습니다. 규칙을 직접 수정해 주세요.",
                json!({"candidate_id": row.id, "blockers": plan.blockers}),
            ));
        }
        for change in plan.changes.iter().filter(|c| c.overwrites) {
            // An automatic actor never confirms an overwrite: a differing
            // existing value is an exception for a human.
            if !decision.confirm_overwrite || auto.is_some() {
                conflicts.push(json!({
                    "candidate_id": row.id, "target": change.target,
                    "current": change.current, "proposed": change.proposed
                }));
            }
        }
        // Apply column and document changes now; rule conditions are
        // collected into one new rule version below.
        for change in &plan.changes {
            match change.target.as_str() {
                "programs.min_age" | "programs.max_age" => {
                    let field = change.target.trim_start_matches("programs.");
                    let age = change.proposed.as_i64().map(|v| v as i32);
                    age_candidate = Some(row.id);
                    changed_fields.insert(field.to_string());
                    if field == "min_age" {
                        state.program.min_age = age;
                    } else {
                        state.program.max_age = age;
                    }
                }
                "programs.application_start_at" | "programs.application_end_at" => {
                    let field = change.target.trim_start_matches("programs.");
                    let at: DateTime<Utc> = serde_json::from_value(change.proposed.clone())
                        .map_err(|e| anyhow::anyhow!("timestamp: {e}"))?;
                    window_candidate = Some(row.id);
                    changed_fields.insert(field.to_string());
                    if field == "application_start_at" {
                        state.program.application_start_at = Some(at);
                    } else {
                        state.program.application_end_at = Some(at);
                    }
                }
                "program_documents" => {
                    let FactValue::Document {
                        name,
                        required,
                        notes,
                    } = &value
                    else {
                        continue;
                    };
                    sqlx::query(
                        "INSERT INTO program_documents (
                             program_id, document_name, description, is_required, sort_order,
                             source, evidence, ai_candidate_id
                         ) VALUES ($1, $2, $3, $4,
                             (SELECT COALESCE(max(sort_order), 0) + 1 FROM program_documents WHERE program_id = $1),
                             $5, $6, $7)",
                    )
                    .bind(program_id)
                    .bind(name)
                    .bind(notes)
                    .bind(required)
                    .bind(actor.document_source())
                    .bind(row.evidence.clone())
                    .bind(row.id)
                    .execute(&mut *conn)
                    .await?;
                    state.documents.push((document_key(name), name.clone()));
                }
                "eligibility_rules.hard_filter_json" => {
                    rule_conditions.push((true, change.proposed.clone()));
                    rule_candidates.push(row.id);
                }
                "eligibility_rules.rule_json" => {
                    rule_conditions.push((false, change.proposed.clone()));
                    rule_candidates.push(row.id);
                }
                _ => {}
            }
        }
        sqlx::query(
            "UPDATE program_ai_extraction_candidates
             SET status = 'approved', reviewer_id = $2, reviewed_at = now(),
                 review_note = $3, applied_changes = $4, applied_by = $5,
                 auto_rule = $6, auto_parser_version = $7, auto_run_id = $8,
                 audit_sample = $9, updated_at = now()
             WHERE id = $1",
        )
        .bind(row.id)
        .bind(reviewer_id)
        .bind(&note)
        .bind(json!({"changes": plan.changes, "notes": plan.notes}))
        .bind(actor.applied_by())
        .bind(auto.map(|p| p.rule.as_str()))
        .bind(auto.map(|p| p.parser_version.as_str()))
        .bind(auto.map(|p| p.run_id))
        .bind(auto.is_some_and(|p| p.audit_sample))
        .execute(&mut *conn)
        .await?;
        results.push(json!({
            "candidate_id": row.id, "decision": "approve",
            "changes": plan.changes, "notes": plan.notes
        }));
    }
    if !conflicts.is_empty() {
        // Nothing is applied: the caller drops the transaction.
        return Err(conflict(
            "overwrite_requires_confirmation",
            "기존 값과 다릅니다. 차이를 확인한 뒤 confirm_overwrite로 다시 승인해 주세요.",
            json!({"conflicts": conflicts}),
        ));
    }

    if state
        .program
        .min_age
        .zip(state.program.max_age)
        .is_some_and(|(min, max)| min > max)
        || state
            .program
            .application_start_at
            .zip(state.program.application_end_at)
            .is_some_and(|(start, end)| start > end)
    {
        return Err(ApplyError::Invalid(
            "approved values contradict the program's other bound".into(),
        ));
    }
    let program = state.program.clone();
    // A reviewer pins both bounds of a changed pair. An automatic actor pins
    // only the field it filled and never replaces a pin that still applies
    // (see `write_override`), so a revert (which deletes its pins) cannot
    // drop a reviewer's approval in force.
    let pinned = |field: &str| auto.is_none() || changed_fields.contains(field);
    let replace = auto.is_none();
    if let Some(candidate) = age_candidate {
        for (field, value) in [("min_age", program.min_age), ("max_age", program.max_age)] {
            if value.is_some() && pinned(field) {
                write_override(
                    conn,
                    program_id,
                    candidate,
                    reviewer_id,
                    field,
                    (value, None),
                    replace,
                )
                .await?;
            }
        }
    }
    if let Some(candidate) = window_candidate {
        for (field, value) in [
            ("application_start_at", program.application_start_at),
            ("application_end_at", program.application_end_at),
        ] {
            if value.is_some() && pinned(field) {
                write_override(
                    conn,
                    program_id,
                    candidate,
                    reviewer_id,
                    field,
                    (None, value),
                    replace,
                )
                .await?;
            }
        }
    }
    sqlx::query(
        "UPDATE programs SET min_age = $2, max_age = $3, application_start_at = $4,
             application_end_at = $5, updated_at = now()
         WHERE id = $1 AND (min_age, max_age, application_start_at, application_end_at)
             IS DISTINCT FROM ($2, $3, $4, $5)",
    )
    .bind(program_id)
    .bind(state.program.min_age)
    .bind(state.program.max_age)
    .bind(state.program.application_start_at)
    .bind(state.program.application_end_at)
    .execute(&mut *conn)
    .await?;

    let mut rule_version = None;
    if !rule_conditions.is_empty() {
        let hard: Vec<Value> = rule_conditions
            .iter()
            .filter(|(h, _)| *h)
            .map(|(_, c)| c.clone())
            .collect();
        let soft: Vec<Value> = rule_conditions
            .iter()
            .filter(|(h, _)| !*h)
            .map(|(_, c)| c.clone())
            .collect();
        let previous = state.rule.as_ref();
        let hard_tree = if hard.is_empty() {
            previous.and_then(|r| r.hard_filter_json.clone())
        } else {
            Some(merge_tree(
                previous.and_then(|r| r.hard_filter_json.clone()),
                &hard,
            ))
        };
        let soft_tree = if soft.is_empty() {
            previous
                .map(|r| r.rule_json.clone())
                .unwrap_or_else(|| json!({"all": hard}))
        } else {
            merge_tree(previous.map(|r| r.rule_json.clone()), &soft)
        };
        for tree in [Some(&soft_tree), hard_tree.as_ref()].into_iter().flatten() {
            mazimi_core::rule_engine::parse_rule_json(tree).map_err(|e| {
                tracing::warn!(%program_id, error = %e, "compiled eligibility rule is invalid");
                ApplyError::Invalid("compiled rule is invalid".into())
            })?;
        }
        let version: i32 = sqlx::query_scalar(
            "INSERT INTO eligibility_rules (
                 program_id, rule_json, hard_filter_json, explain_json, version,
                 created_by, reviewed_by, compiled_at, created_at, updated_at
             ) VALUES ($1, $2, $3, $4,
                 (SELECT COALESCE(max(version), 0) + 1 FROM eligibility_rules WHERE program_id = $1),
                 $5, $6, now(), now(), now())
             RETURNING version",
        )
        .bind(program_id)
        .bind(&soft_tree)
        .bind(&hard_tree)
        // explain_json is public (program detail); carry the previous
        // explanations forward and keep provenance out of it: created_by,
        // reviewed_by (admin id) and the candidates' applied_changes.
        .bind(previous.and_then(|r| r.explain_json.clone()))
        .bind(actor.rule_created_by())
        .bind(reviewer_id.map(|id| id.to_string()))
        .fetch_one(&mut *conn)
        .await?;
        rule_candidates.sort();
        rule_candidates.dedup();
        sqlx::query(
            "UPDATE program_ai_extraction_candidates
             SET applied_changes = applied_changes || jsonb_build_object(
                 'eligibility_rule_version', $2::int, 'previous_rule_id', $3::uuid)
             WHERE id = ANY($1)",
        )
        .bind(&rule_candidates)
        .bind(version)
        .bind(previous.map(|r| r.id))
        .execute(&mut *conn)
        .await?;
        rule_version = Some(version);
    }
    Ok(json!({
        "program_id": program_id,
        "results": results,
        "eligibility_rule_version": rule_version,
    }))
}

async fn write_override(
    conn: &mut PgConnection,
    program_id: Uuid,
    candidate_id: Uuid,
    reviewer_id: Option<Uuid>,
    field: &str,
    (int_value, timestamp_value): (Option<i32>, Option<DateTime<Utc>>),
    replace: bool,
) -> Result<()> {
    // The pin key is the md5 of the stable payload (view counters left out,
    // `ingestion_stable_payload`), so a counter-only change keeps it; a key
    // of the full payload written before still counts as current.
    // An automatic actor never replaces a pin that still applies (same
    // payload). A pin made against an older payload no longer applies at
    // all, so it is replaced; kept, the next sync would reset the value it
    // just filled while the candidate stays `approved`.
    let on_conflict = if replace {
        "DO UPDATE SET
             int_value = EXCLUDED.int_value, timestamp_value = EXCLUDED.timestamp_value,
             raw_payload_md5 = EXCLUDED.raw_payload_md5, candidate_id = EXCLUDED.candidate_id,
             approved_by = EXCLUDED.approved_by, approved_at = now()"
    } else {
        "DO UPDATE SET
             int_value = EXCLUDED.int_value, timestamp_value = EXCLUDED.timestamp_value,
             raw_payload_md5 = EXCLUDED.raw_payload_md5, candidate_id = EXCLUDED.candidate_id,
             approved_by = EXCLUDED.approved_by, approved_at = now()
         WHERE NOT EXISTS (SELECT 1 FROM programs p WHERE p.id = EXCLUDED.program_id
               AND program_ai_field_overrides.raw_payload_md5 IN (
                   md5(ingestion_stable_payload(p.source_type, p.raw_payload)::text),
                   md5(p.raw_payload::text)))"
    };
    sqlx::query(&format!(
        "INSERT INTO program_ai_field_overrides (
             program_id, field, int_value, timestamp_value, raw_payload_md5,
             candidate_id, approved_by, approved_at
         ) VALUES ($1, $2, $3, $4,
             (SELECT md5(ingestion_stable_payload(source_type, raw_payload)::text)
              FROM programs WHERE id = $1), $5, $6, now())
         ON CONFLICT (program_id, field) {on_conflict}"
    ))
    .bind(program_id)
    .bind(field)
    .bind(int_value)
    .bind(timestamp_value)
    .bind(candidate_id)
    .bind(reviewer_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ── Revert ────────────────────────────────────────────────────────────────

/// Which auto-applied facts to undo. Only `applied_by = 'auto_verified'`
/// rows are ever selected; reviewer approvals are corrected by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevertScope {
    Run(Uuid),
    ParserVersion {
        version: String,
        rule: Option<String>,
    },
    Candidate(Uuid),
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RevertReport {
    pub matched: usize,
    pub reverted: Vec<Uuid>,
    /// `{candidate_id, reason}` for facts left applied.
    pub skipped: Vec<Value>,
    /// `{candidate_id, target, reason}` for values kept because they changed
    /// after the automatic application.
    pub kept: Vec<Value>,
    pub dry_run: bool,
    /// More than [`MAX_REVERT`] facts matched; run the same revert again.
    pub truncated: bool,
}

/// Facts reverted per call.
pub const MAX_REVERT: usize = 1000;

#[derive(Debug, FromRow)]
struct AppliedRow {
    id: Uuid,
    program_id: Uuid,
    applied_changes: Option<Value>,
}

/// Undo every auto-applied fact in `scope`, newest first, one transaction
/// per fact. A programs column goes back to its previous value only while it
/// still holds the auto-applied value; a column edited since is kept and
/// reported. Document rows and field overrides written for the fact are
/// removed; a rule version is removed only while it is still the latest.
pub async fn revert(
    pool: &sqlx::PgPool,
    scope: &RevertScope,
    admin_id: Uuid,
    note: &str,
    dry_run: bool,
) -> Result<RevertReport> {
    let (clause, run, version, rule, candidate) = match scope {
        RevertScope::Run(id) => ("auto_run_id = $1", Some(*id), None, None, None),
        RevertScope::ParserVersion { version, rule } => (
            "auto_parser_version = $2 AND ($3::text IS NULL OR auto_rule = $3)",
            None,
            Some(version.clone()),
            rule.clone(),
            None,
        ),
        RevertScope::Candidate(id) => ("id = $4", None, None, None, Some(*id)),
    };
    let mut rows = sqlx::query_as::<_, AppliedRow>(&format!(
        "SELECT id, program_id, applied_changes FROM program_ai_extraction_candidates
         WHERE status = 'approved' AND applied_by = 'auto_verified' AND {clause}
         ORDER BY reviewed_at DESC, id DESC
         LIMIT $5"
    ))
    .bind(run)
    .bind(version)
    .bind(rule)
    .bind(candidate)
    .bind(MAX_REVERT as i64 + 1)
    .fetch_all(pool)
    .await?;
    let truncated = rows.len() > MAX_REVERT;
    rows.truncate(MAX_REVERT);
    let mut report = RevertReport {
        matched: rows.len(),
        dry_run,
        truncated,
        ..RevertReport::default()
    };
    if dry_run {
        return Ok(report);
    }
    for row in rows {
        let mut tx = pool.begin().await?;
        match revert_one(&mut tx, &row, admin_id, note).await {
            Ok(kept) => {
                tx.commit().await?;
                report.reverted.push(row.id);
                report.kept.extend(kept);
            }
            Err(RevertSkip(reason)) => {
                drop(tx);
                report
                    .skipped
                    .push(json!({"candidate_id": row.id, "reason": reason}));
            }
        }
    }
    tracing::info!(%admin_id, matched = report.matched, reverted = report.reverted.len(),
        skipped = report.skipped.len(), "auto-applied ai extraction facts reverted");
    Ok(report)
}

struct RevertSkip(String);

impl<E: std::fmt::Display> From<E> for RevertSkip {
    fn from(error: E) -> Self {
        // Logged in full; the report carries a generic reason only.
        tracing::warn!(error = %error, "auto-apply revert of one fact failed");
        Self("revert failed; nothing was changed for this fact".into())
    }
}

async fn revert_one(
    conn: &mut PgConnection,
    row: &AppliedRow,
    admin_id: Uuid,
    note: &str,
) -> std::result::Result<Vec<Value>, RevertSkip> {
    // Lock the program, then re-read the candidate under the lock.
    let state = load_state(conn, row.program_id, true)
        .await
        .map_err(RevertSkip::from)?;
    let still_applied: bool = sqlx::query_scalar(
        "SELECT status = 'approved' AND applied_by = 'auto_verified'
         FROM program_ai_extraction_candidates WHERE id = $1 FOR UPDATE",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await?;
    if !still_applied {
        return Err(RevertSkip("no longer an applied automatic fact".into()));
    }
    let applied = row.applied_changes.clone().unwrap_or(Value::Null);
    let changes: Vec<Change> = applied
        .get("changes")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let mut kept = Vec::new();
    let mut program = state.program.clone();
    let mut rule_targets = false;
    for change in &changes {
        match change.target.as_str() {
            "programs.min_age" | "programs.max_age" => {
                let slot = if change.target == "programs.min_age" {
                    &mut program.min_age
                } else {
                    &mut program.max_age
                };
                if json!(*slot) == change.proposed {
                    *slot = serde_json::from_value(change.current.clone())?;
                } else {
                    kept.push(json!({"candidate_id": row.id, "target": change.target, "reason": "changed after the automatic application"}));
                }
            }
            "programs.application_start_at" | "programs.application_end_at" => {
                let slot = if change.target == "programs.application_start_at" {
                    &mut program.application_start_at
                } else {
                    &mut program.application_end_at
                };
                let proposed: Option<DateTime<Utc>> =
                    serde_json::from_value(change.proposed.clone())?;
                if *slot == proposed {
                    *slot = serde_json::from_value(change.current.clone())?;
                } else {
                    kept.push(json!({"candidate_id": row.id, "target": change.target, "reason": "changed after the automatic application"}));
                }
            }
            "eligibility_rules.hard_filter_json" | "eligibility_rules.rule_json" => {
                rule_targets = true;
            }
            _ => {}
        }
    }
    if rule_targets {
        let version = applied
            .get("eligibility_rule_version")
            .and_then(Value::as_i64)
            .ok_or_else(|| RevertSkip("applied rule version is not recorded".into()))?;
        let latest_is_ours = state.rule.as_ref().is_some_and(|r| {
            i64::from(r.version) == version && r.created_by.as_deref() == Some("ai_extraction_auto")
        });
        if !latest_is_ours {
            return Err(RevertSkip(
                "the eligibility rule changed after the automatic application; edit it manually"
                    .into(),
            ));
        }
        sqlx::query(
            "DELETE FROM eligibility_rules WHERE program_id = $1 AND version = $2
               AND created_by = 'ai_extraction_auto'",
        )
        .bind(row.program_id)
        .bind(version as i32)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "DELETE FROM program_documents WHERE program_id = $1 AND ai_candidate_id = $2
           AND source = 'ai_extraction_auto'",
    )
    .bind(row.program_id)
    .bind(row.id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM program_ai_field_overrides WHERE candidate_id = $1")
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "UPDATE programs SET min_age = $2, max_age = $3, application_start_at = $4,
             application_end_at = $5, updated_at = now()
         WHERE id = $1 AND (min_age, max_age, application_start_at, application_end_at)
             IS DISTINCT FROM ($2, $3, $4, $5)",
    )
    .bind(row.program_id)
    .bind(program.min_age)
    .bind(program.max_age)
    .bind(program.application_start_at)
    .bind(program.application_end_at)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE program_ai_extraction_candidates
         SET status = 'reverted', reverted_at = now(), reverted_by = $2, revert_note = $3,
             revert_report = $4, updated_at = now()
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(admin_id)
    .bind(note)
    .bind(json!({"kept": kept}))
    .execute(&mut *conn)
    .await?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn state(rule: Option<Value>) -> PublicState {
        PublicState {
            program: ProgramState {
                title: "t".into(),
                min_age: Some(19),
                max_age: None,
                application_start_at: None,
                application_end_at: None,
            },
            rule: rule.map(|tree| EligibilityRule {
                id: Uuid::nil(),
                program_id: Uuid::nil(),
                rule_json: tree.clone(),
                hard_filter_json: Some(tree),
                explain_json: None,
                version: 1,
                created_by: None,
                reviewed_by: None,
                compiled_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }),
            documents: vec![(document_key("신분증"), "신분증".into())],
        }
    }

    use super::super::facts::{AgeBasis, Comparator};

    #[test]
    fn age_plan_fills_gaps_and_flags_overwrites() {
        let plan = plan_fact(
            &FactValue::Age {
                min_age: Some(18),
                max_age: Some(34),
                basis: AgeBasis::ManAge,
            },
            &state(None),
        );
        assert_eq!(plan.changes.len(), 2);
        assert!(
            plan.changes[0].overwrites,
            "19 → 18 overwrites a source value"
        );
        assert_eq!(plan.changes[0].current, json!(19));
        assert!(!plan.changes[1].overwrites);
        let same = plan_fact(
            &FactValue::Age {
                min_age: Some(19),
                max_age: None,
                basis: AgeBasis::ManAge,
            },
            &state(None),
        );
        assert!(same.changes.is_empty());
    }

    #[test]
    fn window_dates_become_kst_day_bounds() {
        let plan = plan_fact(
            &FactValue::ApplicationWindow {
                kind: WindowKind::Dated,
                start_date: NaiveDate::from_ymd_opt(2026, 3, 2),
                end_date: NaiveDate::from_ymd_opt(2026, 4, 30),
            },
            &state(None),
        );
        assert_eq!(plan.changes[0].proposed, json!("2026-03-01T15:00:00Z"));
        assert_eq!(plan.changes[1].proposed, json!("2026-04-30T14:59:59Z"));
    }

    #[test]
    fn rule_facts_compile_to_conditions_and_detect_conflicts() {
        let income = FactValue::Income {
            criterion: IncomeCriterion::IncomeDecile,
            comparator: Comparator::Lte,
            value: 8,
        };
        let fresh = plan_fact(&income, &state(None));
        assert_eq!(fresh.changes.len(), 2);
        assert!(fresh.changes.iter().all(|c| !c.overwrites));
        let existing = json!({"all": [{"field": "income_bracket", "op": "lte", "value": 5}]});
        let conflicting = plan_fact(&income, &state(Some(existing)));
        assert!(conflicting.changes.iter().all(|c| c.overwrites));
        let same =
            json!({"all": [{"field": "income_bracket", "op": "lte", "value": 8, "weight": 2.0}]});
        assert!(plan_fact(&income, &state(Some(same))).changes.is_empty());
        let nested = json!({"any": [{"field": "income_bracket", "op": "lte", "value": 5}, {"field": "age", "op": "lte", "value": 30}]});
        assert!(!plan_fact(&income, &state(Some(nested))).blockers.is_empty());
        let median = FactValue::Income {
            criterion: IncomeCriterion::MedianIncomePercent,
            comparator: Comparator::Lte,
            value: 150,
        };
        let recorded = plan_fact(&median, &state(None));
        assert!(recorded.changes.is_empty() && !recorded.notes.is_empty());
    }

    #[test]
    fn merged_trees_replace_same_field_conditions_and_stay_valid() {
        let tree = json!({"all": [
            {"field": "region_code", "op": "in", "value": ["busan"]},
            {"field": "income_bracket", "op": "lte", "value": 5}
        ]});
        let merged = merge_tree(
            Some(tree),
            &[json!({"field": "income_bracket", "op": "lte", "value": 8, "weight": 1.0})],
        );
        assert_eq!(merged["all"].as_array().unwrap().len(), 2);
        assert_eq!(merged["all"][1]["value"], 8);
        assert!(mazimi_core::rule_engine::parse_rule_json(&merged).is_ok());
        let single = merge_tree(
            Some(json!({"field": "region", "op": "in", "value": ["seoul"]})),
            &[json!({"field": "region_code", "op": "in", "value": ["busan"]})],
        );
        assert_eq!(
            single,
            json!({"all": [{"field": "region_code", "op": "in", "value": ["busan"]}]})
        );
        assert!(mazimi_core::rule_engine::parse_rule_json(&merge_tree(
            None,
            &[json!({"field": "enrollment_status", "op": "in", "value": ["enrolled"]})]
        ))
        .is_ok());
    }

    #[test]
    fn opposite_bounds_on_one_field_coexist() {
        let income = FactValue::Income {
            criterion: IncomeCriterion::IncomeDecile,
            comparator: Comparator::Lte,
            value: 7,
        };
        let range = json!({"all": [
            {"field": "income_bracket", "op": "gte", "value": 2},
            {"field": "income_bracket", "op": "lte", "value": 8}
        ]});
        let plan = plan_fact(&income, &state(Some(range.clone())));
        assert!(plan.changes.iter().all(|c| c.overwrites));
        assert!(plan
            .changes
            .iter()
            .all(|c| c.current == json!([{"field": "income_bracket", "op": "lte", "value": 8}])));
        let merged = merge_tree(
            Some(range),
            &[json!({"field": "income_bracket", "op": "lte", "value": 7})],
        );
        assert_eq!(
            merged,
            json!({"all": [
                {"field": "income_bracket", "op": "gte", "value": 2},
                {"field": "income_bracket", "op": "lte", "value": 7}
            ]}),
            "the lower bound survives"
        );
    }

    #[test]
    fn existing_documents_are_not_duplicated() {
        let plan = plan_fact(
            &FactValue::Document {
                name: "신 분 증".into(),
                required: true,
                notes: None,
            },
            &state(None),
        );
        assert!(plan.changes.is_empty());
        let new = plan_fact(
            &FactValue::Document {
                name: "주민등록등본".into(),
                required: false,
                notes: Some("해당자".into()),
            },
            &state(None),
        );
        assert_eq!(new.changes.len(), 1);
    }

    #[test]
    fn actors_carry_distinct_provenance() {
        let provenance = AutoProvenance {
            run_id: Uuid::nil(),
            rule: "age_range".into(),
            parser_version: PARSER_VERSION.into(),
            audit_sample: true,
        };
        let auto = Actor::Auto(&provenance);
        assert_eq!(auto.reviewer_id(), None);
        assert_eq!(auto.applied_by(), "auto_verified");
        assert_eq!(auto.document_source(), "ai_extraction_auto");
        assert_eq!(auto.rule_created_by(), "ai_extraction_auto");
        assert!(auto.note().starts_with("자동 검증 반영"));
        let id = Uuid::new_v4();
        let reviewer = Actor::Reviewer { id, note: "메모" };
        assert_eq!(reviewer.reviewer_id(), Some(id));
        assert_eq!(reviewer.applied_by(), "reviewer");
        assert_eq!(reviewer.document_source(), "ai_extraction_approved");
    }
}
