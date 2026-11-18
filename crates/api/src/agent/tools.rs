//! Read tools: thin adapters over the existing services. Each returns data,
//! evidence and a small summary for the audit row; none writes. Programme
//! tokens are resolved here against the user's own data and the public
//! availability rule, so a plan cannot reach another user's programmes or a
//! hidden one.
use super::{EvidenceRef, ToolError};
use crate::assistant_provider::{AnswerMode, Topic};
use crate::services::{
    application_preparation, benefit_assistant, benefit_comparison,
    grounded_qa::{self, intent},
};
use axum::http::StatusCode;
use chrono::Utc;
use mazimi_core::agent::{ProgramRef, SearchArgs, ToolCall};
use mazimi_core::models::{CheckEvidence, Evidence};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// Where programme tokens point for one run.
#[derive(Debug, Clone)]
pub struct Scope {
    pub user: Uuid,
    pub context: Option<Uuid>,
    pub selected: Vec<Uuid>,
    /// On-topic programmes found by this run's searches so far, in order
    /// ([`ProgramRef::Found`]). Filled by the executor, never by input.
    pub found: Vec<Uuid>,
}

pub struct Outcome {
    pub data: Value,
    pub evidence: Vec<EvidenceRef>,
    pub summary: Value,
}

pub fn tool_error(code: &str, message: &str) -> ToolError {
    ToolError {
        code: code.into(),
        message: message.into(),
    }
}

/// Map a service error to a user-facing tool error. Internal details stay
/// in the server log.
fn service_error((status, _): (StatusCode, axum::Json<Value>)) -> ToolError {
    match status {
        StatusCode::NOT_FOUND => tool_error(
            "program_unavailable",
            "지금 안내할 수 있는 혜택이 아니에요. 공고 상태를 확인해 주세요.",
        ),
        StatusCode::BAD_REQUEST => tool_error("invalid_request", "요청을 처리할 수 없어요."),
        _ => {
            tracing::warn!(status = status.as_u16(), "agent read tool failed");
            tool_error(
                "tool_failed",
                "정보를 불러오지 못했어요. 잠시 후 다시 시도해 주세요.",
            )
        }
    }
}

fn db_error(error: sqlx::Error) -> ToolError {
    tracing::warn!(error = %error, "agent tool query failed");
    tool_error(
        "tool_failed",
        "정보를 불러오지 못했어요. 잠시 후 다시 시도해 주세요.",
    )
}

/// How many programmes a token may expand to for each tool.
pub fn expansion_limit(call: &ToolCall) -> usize {
    match call {
        ToolCall::Compare(a) if a.programs == ProgramRef::Found => 3,
        ToolCall::Compare(_) => 4,
        // What a search found: the first three, like the answer names.
        ToolCall::SetAlert(a) if a.program == ProgramRef::Found => 3,
        ToolCall::Bookmark(a) if a.program == ProgramRef::Found => 3,
        ToolCall::SetAlert(_) => 5,
        ToolCall::OpenFormFill(_)
        | ToolCall::MatchVaultDocuments(_)
        | ToolCall::RequestSignature(_)
        | ToolCall::OpenOfficialApplication(_) => 1,
        // Each is an outbound request to an official site.
        ToolCall::CheckOfficialPage(_) => 2,
        _ => 3,
    }
}

/// Resolve a token to programme ids. `available_only` applies the same
/// visibility as the detail, assistant and recommendation routes; status
/// recording may name a programme whose intake has closed.
pub async fn resolve(
    pool: &PgPool,
    scope: &Scope,
    reference: ProgramRef,
    max: usize,
    available_only: bool,
) -> Result<Vec<Uuid>, ToolError> {
    let available = mazimi_core::currently_available_sql("p.", "now()");
    // Constants only: the table and filter never come from input.
    let (table, filter) = if available_only {
        ("current_programs", available.as_str())
    } else {
        ("programs", "true")
    };
    let ids: Vec<Uuid> = match reference {
        ProgramRef::Saved => sqlx::query_scalar(&format!(
            "SELECT p.id FROM {table} p WHERE {filter}
               AND (EXISTS(SELECT 1 FROM user_bookmarks b WHERE b.user_id=$1 AND b.program_id=p.id)
                 OR EXISTS(SELECT 1 FROM application_preparations a WHERE a.user_id=$1 AND a.program_id=p.id)
                 OR EXISTS(SELECT 1 FROM alert_subscriptions s WHERE s.user_id=$1 AND s.program_id=p.id AND s.enabled))
             ORDER BY LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) ASC NULLS LAST, p.id
             LIMIT $2"
        ))
        .bind(scope.user)
        .bind(max as i64)
        .fetch_all(pool)
        .await
        .map_err(db_error)?,
        ProgramRef::Context | ProgramRef::Selected | ProgramRef::Found => {
            let wanted: Vec<Uuid> = match reference {
                ProgramRef::Context => scope.context.into_iter().collect(),
                ProgramRef::Found => scope.found.clone(),
                _ => scope.selected.clone(),
            };
            if wanted.is_empty() {
                return Err(match reference {
                    ProgramRef::Context => tool_error(
                        "no_context_program",
                        "어떤 혜택인지 먼저 골라 주세요.",
                    ),
                    ProgramRef::Found => tool_error(
                        "no_found_programs",
                        "앞 단계에서 찾은 혜택이 없어 이 단계는 건너뛰었어요.",
                    ),
                    _ => tool_error("no_selection", "비교할 혜택을 2개 이상 골라 주세요."),
                });
            }
            let sql = if available_only {
                format!("SELECT p.id FROM current_programs p WHERE p.id = ANY($1) AND {available}")
            } else {
                "SELECT p.id FROM programs p WHERE p.id = ANY($1)".to_owned()
            };
            let found: Vec<Uuid> = sqlx::query_scalar(&sql)
                .bind(&wanted)
                .fetch_all(pool)
                .await
                .map_err(db_error)?;
            wanted
                .into_iter()
                .filter(|id| found.contains(id))
                .take(max)
                .collect()
        }
    };
    if ids.is_empty() {
        return Err(match reference {
            ProgramRef::Saved => tool_error(
                "no_saved_programs",
                "저장하거나 준비 중인 혜택이 아직 없어요. 먼저 혜택을 찾아 저장해 주세요.",
            ),
            _ => tool_error(
                "program_unavailable",
                "지금 안내할 수 있는 혜택이 아니에요. 공고 상태를 확인해 주세요.",
            ),
        });
    }
    Ok(ids)
}

pub struct EvidenceIds {
    seq: i32,
    next: usize,
}

impl EvidenceIds {
    pub fn new(seq: i32) -> Self {
        Self { seq, next: 0 }
    }
    pub(crate) fn id(&mut self) -> String {
        let id = format!("{}.{}", self.seq, self.next);
        self.next += 1;
        id
    }
    fn cite(&mut self, program: Uuid, evidence: &Evidence) -> EvidenceRef {
        EvidenceRef {
            id: self.id(),
            program_id: Some(program),
            kind: serde_json::to_value(evidence.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "official_text".into()),
            quote: evidence.quote.clone(),
            source_label: evidence.source_label.clone(),
            source_url: evidence.source_url.clone(),
            captured_at: evidence.captured_at,
        }
    }
    fn cite_check(&mut self, program: Uuid, evidence: &CheckEvidence) -> EvidenceRef {
        self.cite(program, &evidence.evidence)
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

pub async fn search_benefits(
    pool: &PgPool,
    user: Uuid,
    args: &SearchArgs,
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    search_benefits_cached(pool, user, args, ids, &mut None).await
}

/// [`search_benefits`] reusing the run's candidate list (a relaxed re-search
/// scores nothing again).
pub async fn search_benefits_cached(
    pool: &PgPool,
    user: Uuid,
    args: &SearchArgs,
    ids: &mut EvidenceIds,
    cache: &mut benefit_assistant::search::SearchCache,
) -> Result<Outcome, ToolError> {
    let intent = intent::from_parts(args);
    let search = benefit_assistant::search::search_with_cache(pool, user, intent, cache)
        .await
        .map_err(service_error)?;
    let mut evidence = Vec::new();
    let results: Vec<Value> = search
        .results
        .iter()
        .map(|r| {
            evidence.extend(
                r.item
                    .evidence
                    .iter()
                    .take(2)
                    .map(|e| ids.cite_check(r.item.program_id, e)),
            );
            // A corporate offer's amounts are no support money.
            let paid = mazimi_core::pays_support(&r.item.program_type);
            json!({
                "program_id": r.item.program_id,
                "title": r.item.title,
                "title_display": mazimi_core::title_display::title_display(&r.item.title),
                "program_type": r.item.program_type,
                "field_category": r.item.categories.field_category,
                "field_label": r.item.categories.field_label,
                "provider_label": r.item.categories.provider_label,
                "topic_match": r.topic_match,
                "deadline_days": r.deadline_days,
                "local": r.local,
                "outside_region": r.outside_region,
                "amount_unregistered": r.amount_unregistered,
                "fit_reasons": super::compose::fit_reasons(&r.item.reasons),
                "remaining_checks": r.item.remaining_checks.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),
                // What is still to check, said in the answer: the reviewed
                // condition kinds only, not the review's own state ("공고의
                // 자격 조건 검토가 필요해요" says nothing to check by itself).
                "to_check": r.item.remaining_checks.iter()
                    .filter(|c| mazimi_core::condition_review::is_check_kind(&c.kind))
                    .map(|c| c.label.clone()).collect::<Vec<_>>(),
                "verified": r.item.verified,
                "eligibility_status": r.item.eligibility_status,
                "deadline": r.item.deadline,
                "benefit_amount_monthly": r.item.benefit_amount_monthly.filter(|_| paid),
                "benefit_amount_semester": r.item.benefit_amount_semester.filter(|_| paid),
                "reasons": r.item.reasons,
                "missing_checks": r.item.missing_checks,
                "matched_keywords": r.matched_keywords,
                "official_url": r.item.official_url,
            })
        })
        .collect();
    Ok(Outcome {
        summary: json!({"results": results.len(), "total_candidates": search.total_candidates,
            "not_found": search.not_found}),
        data: json!({
            "intent": search.intent,
            "results": results,
            "filter_summary": search.filter_summary,
            "total_candidates": search.total_candidates,
            "required_profile_fields": search.required_profile_fields,
            "not_found": search.not_found,
            "notice": search.notice,
            "topic": search.topic,
            "region_label": search.region_label,
            "saved_region_label": search.saved_region_label,
            "region_basis": search.region_basis,
            "unmatched_words": search.unmatched_words,
            "args": args,
        }),
        evidence,
    })
}

pub async fn check_eligibility(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    let guidance = benefit_assistant::guidance(
        pool,
        user,
        program,
        &[Topic::Eligibility],
        AnswerMode::Rules,
        false,
        None,
    )
    .await
    .map_err(service_error)?;
    let mut evidence = Vec::new();
    let checks: Vec<Value> = guidance
        .eligibility
        .checks
        .iter()
        .map(|c| {
            evidence.extend(c.evidence.iter().map(|e| ids.cite(program, e)));
            json!({"label": c.label, "status": c.status, "detail": c.detail, "bundle": c.bundle})
        })
        .collect();
    Ok(Outcome {
        summary: json!({"status": guidance.eligibility.status, "checks": checks.len()}),
        data: json!({
            "program_id": program,
            "title": guidance.title,
            "title_display": mazimi_core::title_display::title_display(&guidance.title),
            "status": guidance.eligibility.status,
            "summary": guidance.eligibility.summary,
            "checks": checks,
            "missing_fields": guidance.eligibility.missing_fields,
            "deadline_at": guidance.deadline_at,
            "alert_enabled": guidance.alert_enabled,
            "official_url": guidance.source.official_url,
        }),
        evidence,
    })
}

pub async fn answer_from_notice(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
    question: &str,
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    let (topics, _) = grounded_qa::topics_for(question, None);
    let guidance = benefit_assistant::guidance(
        pool,
        user,
        program,
        &topics,
        AnswerMode::Rules,
        false,
        Some(benefit_assistant::Asked {
            question,
            previous_topic: None,
        }),
    )
    .await
    .map_err(service_error)?;
    let answer = guidance
        .answer
        .ok_or_else(|| tool_error("tool_failed", "공고 문장을 찾지 못했어요."))?;
    let official_url = guidance.source.official_url.clone();
    // A documents question answers from the required documents' one source
    // (the detail page's list), with the notice's documents text as its
    // evidence, never from the passages a retrieval picked (QA B1).
    let documents = if topics.contains(&Topic::Documents) {
        let mut connection = pool.acquire().await.map_err(db_error)?;
        Some(required_documents_of(&mut connection, program).await?)
    } else {
        None
    };
    let mut evidence: Vec<EvidenceRef> = Vec::new();
    if let Some(text) = documents.as_ref().and_then(|d| d.text.as_deref()) {
        evidence.push(EvidenceRef {
            id: ids.id(),
            program_id: Some(program),
            kind: "official_text".into(),
            quote: Some(truncate(text, 600)),
            source_label: "제출서류".into(),
            source_url: official_url.clone(),
            captured_at: None,
        });
    }
    let listed = documents
        .as_ref()
        .is_some_and(|d| !d.items.is_empty() || d.none_required);
    // The passages answer the other topics; a documents list replaces the
    // documents passages, which quote the same field.
    evidence.extend(
        answer
            .passages
            .iter()
            .filter(|p| !(listed && p.section.topic() == Some(Topic::Documents)))
            .map(|p| EvidenceRef {
                id: ids.id(),
                program_id: Some(program),
                kind: serde_json::to_value(p.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "official_text".into()),
                quote: Some(p.text.clone()),
                source_label: p.source_label.clone(),
                source_url: official_url.clone(),
                captured_at: p.captured_at,
            }),
    );
    let documents_json = documents.as_ref().filter(|_| listed).map(|d| {
        json!({
            "items": d.items.iter().map(|i| json!({
                "name": i.document_name, "description": i.description, "required": i.required(),
            })).collect::<Vec<_>>(),
            "notes": d.notes,
            "none_required": d.none_required,
        })
    });
    Ok(Outcome {
        summary: json!({"passages": answer.passages.len(), "not_found": answer.not_found,
            "unsupported": answer.unsupported,
            "documents": documents.as_ref().map(|d| d.items.len())}),
        data: json!({
            "program_id": program,
            "title": guidance.title,
            "answer": answer,
            "documents": documents_json,
            "eligibility_status": guidance.eligibility.status,
            "official_url": official_url,
        }),
        evidence,
    })
}

pub async fn get_preparation(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    let mut connection = pool.acquire().await.map_err(db_error)?;
    let plan = application_preparation::load_on(&mut connection, user, program)
        .await
        .map_err(service_error)?;
    let remaining: Vec<&application_preparation::Task> = plan
        .tasks
        .iter()
        .filter(|t| !plan.completed_tasks.contains(&t.id))
        .collect();
    let evidence: Vec<EvidenceRef> = plan
        .official_links
        .iter()
        .map(|link| EvidenceRef {
            id: ids.id(),
            program_id: Some(program),
            kind: "official_text".into(),
            quote: Some(truncate(&link.evidence, 300)),
            source_label: "공고 신청 방법".into(),
            source_url: crate::services::benefit_assistant::safe_official_url(Some(&link.url)),
            captured_at: None,
        })
        .collect();
    Ok(Outcome {
        summary: json!({"tasks": plan.tasks.len(), "remaining": remaining.len(),
            "progress_reset": plan.progress_reset}),
        data: json!({
            "program_id": program,
            "title": plan.title,
            "revision": plan.revision,
            "progress_reset": plan.progress_reset,
            "tasks": plan.tasks.iter().map(|t| json!({
                "id": t.id, "title": t.title, "detail": truncate(&t.detail, 300), "kind": t.kind,
                "done": plan.completed_tasks.contains(&t.id),
            })).collect::<Vec<_>>(),
            "remaining": remaining.len(),
            "total": plan.tasks.len(),
            "next_task": remaining.first().map(|t| t.title.clone()),
            "selected_channel": plan.selected_channel,
            "can_prepare": plan.policy.can_prepare,
            "has_template": !plan.policy.resources.templates.is_empty(),
        }),
        evidence,
    })
}

pub async fn compare(
    pool: &PgPool,
    user: Uuid,
    programs: &[Uuid],
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    if programs.len() < 2 {
        return Err(tool_error(
            "not_enough_programs",
            "비교하려면 혜택이 2개 이상 필요해요. 혜택을 더 저장하거나 골라 주세요.",
        ));
    }
    let comparison = benefit_comparison::compare(pool, user, programs)
        .await
        .map_err(service_error)?;
    let mut evidence = Vec::new();
    let rows: Vec<Value> = comparison
        .programs
        .iter()
        .map(|g| {
            evidence.extend(
                g.eligibility
                    .checks
                    .iter()
                    .flat_map(|c| c.evidence.iter())
                    .take(2)
                    .map(|e| ids.cite(g.program_id, e)),
            );
            json!({
                "program_id": g.program_id,
                "title": g.title,
                "eligibility_status": g.eligibility.status,
                "eligibility_summary": g.eligibility.summary,
                "deadline_at": g.deadline_at,
                "amounts": {"monthly": g.amounts.monthly, "semester": g.amounts.semester, "once": g.amounts.once},
                "remaining_tasks": g.remaining_task_ids.len(),
                "total_tasks": g.preparation.tasks.len(),
                "official_url": g.source.official_url,
            })
        })
        .collect();
    Ok(Outcome {
        summary: json!({"programs": rows.len(), "shared_documents": comparison.shared_documents.len()}),
        data: json!({
            "programs": rows,
            "shared_documents": comparison.shared_documents.iter().map(|d| json!({
                "name": d.name, "programs": d.uses.iter().map(|u| u.program_id).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "compatibility_notice": comparison.compatibility_notice,
            "documents_notice": comparison.documents_notice,
            "amounts_additive": comparison.amounts_additive,
        }),
        evidence,
    })
}

pub async fn deadlines(
    pool: &PgPool,
    user: Uuid,
    args: &mazimi_core::agent::DeadlineArgs,
) -> Result<Outcome, ToolError> {
    let within_days = args.within_days;
    let briefing = benefit_assistant::briefing(pool, user)
        .await
        .map_err(service_error)?;
    // The period's Korean calendar days, as the search reads them (audit M2).
    let today = mazimi_core::seoul_date(Utc::now());
    let period = within_days.map(|to| {
        let day = |offset: u16| today + chrono::Days::new(u64::from(offset));
        (day(args.from_days.unwrap_or(0)), day(to))
    });
    let items: Vec<Value> = briefing
        .items
        .iter()
        .filter(|i| {
            period.is_none_or(|(from, to)| {
                i.deadline_at
                    .map(mazimi_core::deadline_date)
                    .is_some_and(|d| (from..=to).contains(&d))
            })
        })
        .map(|i| {
            json!({
                "program_id": i.program_id,
                "title": i.title,
                "deadline_at": i.deadline_at,
                "reason": i.reason,
                "next_task": i.next_task,
                "remaining_count": i.remaining_count,
                "progress_reset": i.progress_reset,
            })
        })
        .collect();
    Ok(Outcome {
        summary: json!({"items": items.len()}),
        data: json!({"items": items, "within_days": within_days, "scope": briefing.scope}),
        evidence: Vec::new(),
    })
}

pub async fn open_form_fill(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
) -> Result<Outcome, ToolError> {
    let mut connection = pool.acquire().await.map_err(db_error)?;
    let plan = application_preparation::load_on(&mut connection, user, program)
        .await
        .map_err(service_error)?;
    // In-app deep links only: the form-fill screen loads the template by id
    // and re-validates it. No external URL is produced here.
    let forms: Vec<Value> = plan
        .policy
        .resources
        .templates
        .iter()
        .map(|t| {
            json!({
                "resource_id": t.id,
                "label": t.label,
                "file_format": t.file_format,
                "route": "/form-fill",
                "params": {"programId": program, "resourceId": t.id},
            })
        })
        .collect();
    Ok(Outcome {
        summary: json!({"forms": forms.len()}),
        data: json!({
            "program_id": program,
            "title": plan.title,
            "forms": forms,
            "can_autofill": plan.policy.can_autofill,
        }),
        evidence: Vec::new(),
    })
}

/// Document names that mean the same paper ("등본" is "주민등록표 등본"), as
/// keys, with the device vault's type for it. More specific papers come
/// first: "건강보험료 납부확인서(소득증명용)" is a health-insurance paper, not
/// a 소득금액증명원 (code review H2).
const DOCUMENT_KEYS: &[(&str, Option<&str>, &[&str])] = &[
    (
        "health_insurance",
        None,
        &[
            "건강보험자격득실확인서",
            "자격득실확인서",
            "건강보험자격득실",
        ],
    ),
    (
        "health_premium",
        Some("health_insurance"),
        &["건강보험료납부확인서", "보험료납부확인서"],
    ),
    (
        "resident_copy",
        Some("resident_register"),
        &["주민등록등본", "주민등록표등본", "등본"],
    ),
    (
        "resident_abstract",
        None,
        &["주민등록초본", "주민등록표초본", "초본"],
    ),
    (
        "family_relation",
        Some("family_relation"),
        &["가족관계증명서", "가족관계증명"],
    ),
    (
        "enrollment",
        Some("enrollment_cert"),
        &["재학증명서", "재학증명"],
    ),
    ("leave", Some("leave_cert"), &["휴학증명서"]),
    (
        "graduation",
        Some("graduation_cert"),
        &["졸업증명서", "졸업예정증명서"],
    ),
    ("transcript", None, &["성적증명서"]),
    (
        "income",
        Some("income_proof"),
        &["소득금액증명원", "소득금액증명", "소득증명"],
    ),
    ("bank_book", Some("bank_statement"), &["통장사본", "통장"]),
    ("disability", Some("disability_cert"), &["장애인증명서"]),
    ("id_card", None, &["신분증", "신분증사본"]),
    ("lease", None, &["임대차계약서", "전월세계약서"]),
];

fn compact(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn document_entry(
    name: &str,
) -> Option<&'static (&'static str, Option<&'static str>, &'static [&'static str])> {
    let compact = compact(name);
    DOCUMENT_KEYS.iter().find(|(_, _, names)| {
        names
            .iter()
            .any(|n| compact == *n || (n.chars().count() >= 4 && compact.contains(n)))
    })
}

fn document_key(name: &str) -> String {
    document_entry(name).map_or_else(|| compact(name), |(key, _, _)| (*key).to_owned())
}

/// Whether two document names are the same paper. Containment counts only
/// through the aliases ([`DOCUMENT_KEYS`]); other names must be the same once
/// spaces and marks are gone, so a "XX신청서" never stands for "신청서".
pub(crate) fn same_document(a: &str, b: &str) -> bool {
    document_key(a) == document_key(b)
}

/// Words that make a required paper someone else's (부모, 배우자 …) or a
/// particular form of it (상세, 확정일자 …): a name match alone never makes it
/// ready (code review H2).
const SUBJECTS: &[&str] = &[
    "부모",
    "부(",
    "모(",
    "부)",
    "모)",
    "부의",
    "모의",
    "父",
    "母",
    "배우자",
    "세대원",
    "가구원",
    "보호자",
    "형제",
    "자매",
    "자녀",
    "본인및",
];
const QUALIFIERS: &[&str] = &[
    "상세",
    "확정일자",
    "원본",
    "공증",
    "번역",
    "아포스티유",
    "직인",
];

/// The name without copy counts ("1부", "각 2부"): `부` there is no father
/// (re-review L1).
fn without_copy_counts(name: &str) -> String {
    static COUNT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(각\s*)?\d+\s*부").unwrap());
    COUNT.replace_all(name, " ").into_owned()
}

/// Whether `name` names someone else's paper or a particular form of it.
pub(crate) fn qualified_document(name: &str) -> bool {
    let plain: String = without_copy_counts(name)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    SUBJECTS.iter().any(|s| plain.contains(s)) || QUALIFIERS.iter().any(|q| plain.contains(q))
}

/// Whether a required paper (its name or the notice's detail for it) states
/// any issue condition: a digit window, or 발급 with 이내 / 이후 / 발급분 /
/// 발급본 / 신청월 / 당월 / 한 달 / 두 달 (re-review M1). Such a paper is never
/// ready from a checklist mark: only an issue date can show it qualifies.
pub(crate) fn issue_condition(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    issue_window_days(text).is_some()
        || (compact.contains("발급")
            && [
                "이내",
                "이후",
                "발급분",
                "발급본",
                "신청월",
                "당월",
                "한달",
                "두달",
            ]
            .iter()
            .any(|w| compact.contains(w)))
}

/// The issue window a required paper states ("최근 3개월 이내 발급",
/// "발급일로부터 1개월 이내", "30일 이내"), in days.
pub(crate) fn issue_window_days(name: &str) -> Option<u32> {
    static WINDOW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(\d{1,3})\s*(개월|달|일|주)\s*(이내|안|내)").unwrap()
    });
    let c = WINDOW.captures(name)?;
    let n: u32 = c[1].parse().ok()?;
    Some(match &c[2] {
        "개월" | "달" => n * 31,
        "주" => n * 7,
        _ => n,
    })
}

/// The programme's required documents ([`crate::services::required_documents`]).
async fn required_documents_of(
    connection: &mut sqlx::PgConnection,
    program: Uuid,
) -> Result<crate::services::required_documents::RequiredDocuments, ToolError> {
    let row =
        sqlx::query_as::<_, mazimi_core::models::Program>("SELECT * FROM programs WHERE id=$1")
            .bind(program)
            .fetch_optional(&mut *connection)
            .await
            .map_err(db_error)?
            .ok_or_else(|| {
                tool_error(
                    "program_unavailable",
                    "지금 안내할 수 있는 혜택이 아니에요. 공고 상태를 확인해 주세요.",
                )
            })?;
    crate::services::required_documents::load(connection, &row)
        .await
        .map_err(db_error)
}

/// `match_vault_documents`: the programme's required documents against the
/// checklist entries the user marked (`user_document_items`: names and
/// statuses only). The vault itself is on the device: the mobile card
/// matches the same list against it (type, name, issue and expiry dates),
/// so no file and no vault entry reaches the server. Each paper is `have`
/// (marked done, a plain paper), `needs_check` (marked done, but someone
/// else's, a particular form, or with an issue window the list cannot
/// confirm) or `missing`; `type_hint` and `window_days` let the device
/// match.
pub async fn match_vault_documents(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
) -> Result<Outcome, ToolError> {
    let mut connection = pool.acquire().await.map_err(db_error)?;
    let plan = application_preparation::load_on(&mut connection, user, program)
        .await
        .map_err(service_error)?;
    // The required documents' one source (the detail page's list, the
    // checklist's document tasks); optional items are not compared.
    let documents = required_documents_of(&mut connection, program).await?;
    let required: Vec<(String, String)> = documents
        .items
        .iter()
        .filter(|d| d.required())
        .map(|d| {
            (
                d.document_name.clone(),
                d.description.clone().unwrap_or_default(),
            )
        })
        // The same bound as before on a list split from free text.
        .take(30)
        .collect();
    let required_from = documents.items.first().map(|d| d.source);
    // Newest first (security review I2).
    let marked: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM user_document_items
         WHERE user_id=$1 AND (program_id IS NULL OR program_id=$2) AND status='done'
         ORDER BY created_at DESC LIMIT 200",
    )
    .bind(user)
    .bind(program)
    .fetch_all(&mut *connection)
    .await
    .map_err(db_error)?;
    let (mut have, mut needs_check, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    let rows: Vec<Value> = required
        .iter()
        .map(|(title, detail)| {
            let text = format!("{title} {detail}");
            let qualified = qualified_document(title);
            let window = issue_window_days(&text);
            let condition = issue_condition(&text);
            // A mark counts only for the same subject (re-review M2): the
            // parent's paper never stands for the applicant's, or back.
            let same = |name: &String| same_document(name, title);
            let exact = marked
                .iter()
                .any(|name| same(name) && qualified_document(name) == qualified);
            let other = marked.iter().any(same);
            let status = if exact && !qualified && !condition {
                "have"
            } else if exact || other {
                "needs_check"
            } else {
                "missing"
            };
            match status {
                "have" => have.push(title.clone()),
                "needs_check" => needs_check.push(title.clone()),
                _ => missing.push(title.clone()),
            }
            json!({
                "name": title,
                "detail": detail,
                "status": status,
                "type_hint": document_entry(title).and_then(|(_, hint, _)| *hint),
                "qualified": qualified,
                "window_days": window,
                "issue_condition": condition,
            })
        })
        .collect();
    Ok(Outcome {
        summary: json!({"required": rows.len(), "have": have.len(), "needs_check": needs_check.len(), "missing": missing.len()}),
        data: json!({
            "program_id": program,
            "title": plan.title,
            "title_display": mazimi_core::title_display::title_display(&plan.title),
            "documents": rows,
            "have": have,
            "needs_check": needs_check,
            "missing": missing,
            "no_document_list": required.is_empty(),
            "required_from": required_from,
            "compared_with": "checklist",
        }),
        evidence: Vec::new(),
    })
}

/// Run one read tool on one resolved programme (or none).
pub async fn run_read(
    pool: &PgPool,
    user: Uuid,
    call: &ToolCall,
    programs: &[Uuid],
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    let first = || {
        programs
            .first()
            .copied()
            .ok_or_else(|| tool_error("no_context_program", "어떤 혜택인지 먼저 골라 주세요."))
    };
    match call {
        ToolCall::SearchBenefits(args) => search_benefits(pool, user, args, ids).await,
        ToolCall::CheckEligibility(_) => check_eligibility(pool, user, first()?, ids).await,
        ToolCall::AnswerFromNotice(args) => {
            answer_from_notice(pool, user, first()?, &args.question, ids).await
        }
        ToolCall::GetPreparation(_) => get_preparation(pool, user, first()?, ids).await,
        ToolCall::Compare(_) => compare(pool, user, programs, ids).await,
        ToolCall::Deadlines(args) => deadlines(pool, user, args).await,
        ToolCall::OpenFormFill(_) => open_form_fill(pool, user, first()?).await,
        ToolCall::MatchVaultDocuments(_) => match_vault_documents(pool, user, first()?).await,
        ToolCall::DocumentIssueLink(args) => {
            super::documents::issue_links(
                pool,
                user,
                programs.first().copied(),
                args.document.as_deref(),
            )
            .await
        }
        // Needs the runtime's fetch client: `run.rs` calls `official_page::check`.
        ToolCall::CheckOfficialPage(_) => Err(tool_error(
            "tool_failed",
            "정보를 불러오지 못했어요. 잠시 후 다시 시도해 주세요.",
        )),
        ToolCall::AddToCalendar(_)
        | ToolCall::Bookmark(_)
        | ToolCall::SetAlert(_)
        | ToolCall::ToggleChecklistItem(_)
        | ToolCall::RecordApplicationStatus(_)
        | ToolCall::RequestSignature(_)
        | ToolCall::OpenOfficialApplication(_)
        | ToolCall::ProfilePatch(_) => Err(tool_error(
            "write_requires_confirmation",
            "변경은 확인을 거쳐야 해요.",
        )),
    }
}

#[cfg(test)]
mod document_match_tests {
    use super::{issue_condition, issue_window_days, qualified_document, same_document};

    #[test]
    fn documents_match_by_alias_or_exact_name() {
        assert!(same_document(
            "주민등록초본(주소변동사항 포함) 1부",
            "주민등록초본"
        ));
        assert!(same_document("주민등록표 등본", "등본"));
        assert!(same_document("재학 증명서", "재학증명서"));
        assert!(
            !same_document("신청서", "XX장학 신청서.hwp"),
            "code review M8"
        );
        assert!(!same_document("서약서", "개인정보 동의 서약서"));
        assert!(!same_document("재학증명서", "졸업증명서"));
        // Code review H2: the more specific paper wins.
        assert!(!same_document(
            "건강보험료 납부확인서(소득증명용)",
            "소득금액증명원"
        ));
    }

    /// Code review H2: someone else's paper, a particular form or an issue
    /// window is never ready from a name match.
    #[test]
    fn qualified_papers_and_issue_windows_are_read() {
        for name in [
            "부모 소득금액증명원",
            "부(父) 주민등록등본",
            "가족관계증명서(상세)",
            "확정일자가 날인된 임대차계약서",
            "배우자 재직증명서",
            "세대원 전원 건강보험료 납부확인서",
        ] {
            assert!(qualified_document(name), "{name}");
        }
        for name in [
            "소득금액증명원",
            "주민등록등본",
            "가족관계증명서",
            "재학증명서",
        ] {
            assert!(!qualified_document(name), "{name}");
        }
        assert_eq!(
            issue_window_days("주민등록등본(최근 1개월 이내 발급)"),
            Some(31)
        );
        assert_eq!(
            issue_window_days("발급일로부터 3개월 이내 재학증명서"),
            Some(93)
        );
        assert_eq!(issue_window_days("신청일 기준 30일 이내 발급분"), Some(30));
        assert_eq!(issue_window_days("주민등록등본 1부"), None);
        // Re-review M1: issue conditions without a digit window.
        for text in [
            "주민등록등본(신청일 기준 한 달 이내 발급분)",
            "주민등록초본 1부 *공고일 이후 발급분(주소변동사항 포함)만 인정",
            "건강보험자격득실확인서(전체자격 조회본, 시험 응시일 이후 신청 월 발급본)(필수)",
            "주민등록초본 1부 *지원금 신청기간 내 발급분",
            "주민등록초본(신청월 발급분, 주소변동사항 포함)",
        ] {
            assert!(issue_condition(text), "{text}");
        }
        assert!(!issue_condition("주민등록초본(주소변동사항 포함) 1부"));
        // Re-review L1: a copy count is no father.
        for name in [
            "본인명의 통장사본 1부(필수)",
            "임대차계약서 사본 1부(필수)",
            "성적증명서(국/영문 각 1부)",
        ] {
            assert!(!qualified_document(name), "{name}");
        }
        // Re-review M2: both sides' qualification is compared in
        // `match_vault_documents`, so neither stands for the other.
        for (a, b) in [
            ("부모 소득금액증명원", "소득금액증명원"),
            ("배우자 가족관계증명서", "가족관계증명서"),
            ("부(父) 주민등록등본", "주민등록등본"),
        ] {
            assert!(same_document(a, b) && qualified_document(a) != qualified_document(b));
        }
    }
}
