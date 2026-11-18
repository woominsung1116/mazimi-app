//! Shared versioned workflow used by HTTP handlers and the benefit assistant.
use super::application_policy;
use axum::{http::StatusCode, Json};
use mazimi_core::{models::Program, source_policy::canonicalize_public_https_url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub type Error = (StatusCode, Json<Value>);
#[derive(Debug, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub kind: &'static str,
}
/// An HTTPS link quoted in the notice's application-method text. It is a
/// pointer the user may open, never proof of a working form or submit authority.
#[derive(Serialize)]
pub struct OfficialLink {
    pub url: String,
    pub evidence: String,
}
#[derive(Serialize)]
pub struct Plan {
    pub revision: String,
    pub title: String,
    pub policy: application_policy::ApplicationPolicy,
    pub tasks: Vec<Task>,
    pub completed_tasks: Vec<String>,
    pub progress_reset: bool,
    pub selected_channel: Option<String>,
    pub official_links: Vec<OfficialLink>,
}
const MAX_OFFICIAL_LINKS: usize = 3;

/// Extract up to three distinct HTTPS links from the application-method text.
/// Only public hosts without credentials pass; the line is kept as evidence.
/// A link is stored as written (fragments may be SPA routes, query bytes may
/// be EUC-KR) and compared by canonical key, so `?b=1&a=2#x` and `?a=2&b=1`
/// are one link. Canonicalization is validation and dedupe only.
fn official_links(method: Option<&str>) -> Vec<OfficialLink> {
    let mut links: Vec<OfficialLink> = vec![];
    let mut seen: Vec<String> = vec![];
    for line in method.unwrap_or("").lines().take(400) {
        for (start, _) in line.match_indices("https://") {
            if links.len() >= MAX_OFFICIAL_LINKS {
                return links;
            }
            let value: String = line[start..]
                .chars()
                .take_while(|c| {
                    !c.is_whitespace() && !['<', '>', '"', '\'', '（', '）'].contains(c)
                })
                .take(2049)
                .collect();
            let value = value.trim_end_matches([')', ']', '}', ',', ';', '.']);
            if value.len() > 2048 {
                continue;
            }
            let Ok(canonical) = canonicalize_public_https_url(value) else {
                continue;
            };
            if seen.contains(&canonical.canonical_url) {
                continue;
            }
            seen.push(canonical.canonical_url);
            links.push(OfficialLink {
                url: value.to_owned(),
                evidence: line.trim().chars().take(600).collect(),
            });
        }
    }
    links
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavePreparation {
    revision: String,
    completed_tasks: Vec<String>,
    selected_channel: Option<String>,
}

impl SavePreparation {
    pub(crate) fn new(
        revision: String,
        completed_tasks: Vec<String>,
        selected_channel: Option<String>,
    ) -> Self {
        Self {
            revision,
            completed_tasks,
            selected_channel,
        }
    }
}

pub(crate) async fn build(pool: &PgPool, id: Uuid) -> Result<Plan, Error> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(crate::errors::internal_error)?;
    build_on(&mut connection, id).await
}

pub(crate) async fn build_on(connection: &mut sqlx::PgConnection, id: Uuid) -> Result<Plan, Error> {
    // Same availability rule as the assistant, compare and recommendations:
    // a checklist is not offered for an upcoming or closed intake.
    let p = sqlx::query_as::<_, Program>(&format!(
        "SELECT * FROM current_programs WHERE id = $1 AND {}",
        mazimi_core::currently_available_sql("", "now()")
    ))
    .bind(id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"현재 이용 가능한 혜택을 찾을 수 없습니다."})),
        )
    })?;
    // The one source of required documents (the detail page's list); an
    // item marked (선택) is not a task.
    let documents = super::required_documents::load(&mut *connection, &p)
        .await
        .map_err(crate::errors::internal_error)?;
    let (resources, identity) = super::application_resources::load_with_identity(connection, &p)
        .await
        .map_err(crate::errors::internal_error)?;
    let policy = application_policy::with_resources(&p, resources);
    // The same policy as if no verification had expired: the revision hashes
    // identity, so expiry and re-approval of the same entry keep progress.
    let stable = application_policy::with_resources(&p, identity);
    let official_links = official_links(p.application_method.as_deref());
    let mut tasks = vec![Task {
        id: "eligibility".into(),
        title: if policy.workflow.no_application_required {
            "이용 대상 확인".into()
        } else {
            "신청 조건 확인".into()
        },
        detail: p
            .normalized_payload
            .as_ref()
            .and_then(|v| v.get("conditions"))
            .and_then(Value::as_str)
            .unwrap_or("공식 공고의 대상·소득·중복 수혜 조건을 확인해 주세요.")
            .into(),
        kind: "eligibility",
    }];
    if policy.workflow.no_application_required {
        tasks.push(Task { id:"access_conditions".into(),title:"혜택 이용 조건 확인".into(),detail:"별도 신청이 필요하지 않다는 안내를 확인하고, 실제 이용 대상과 이용 절차를 확인해 주세요.".into(),kind:"information_check" });
    } else {
        let required: Vec<_> = documents.items.iter().filter(|d| d.required()).collect();
        for doc in &required {
            tasks.push(Task {
                id: format!("document:{}", doc.document_name),
                title: doc.document_name.clone(),
                detail: doc
                    .description
                    .clone()
                    .unwrap_or_else(|| "공고에 요구된 발급일과 제출 형식을 확인해 주세요.".into()),
                kind: "document",
            });
        }
        if documents.items.is_empty() {
            // The notice states nothing is to be submitted: a check, not an
            // unknown list (no 서류 보관함 link).
            let none = documents.none_required;
            tasks.push(Task{id:"documents:confirm".into(),title:"제출 서류 확인".into(),
            detail:if none {
                "공고상 별도 제출 서류 없음".into()
            } else {
                p.submission_documents.clone().filter(|s|!s.trim().is_empty()).unwrap_or_else(||"확인된 서류 목록이 아직 없어요. 공식 신청처에서 제출 대상과 서류를 확인해 주세요.".into())
            },kind:if none {"information_check"} else {"documents_unknown"}});
        }
        tasks.push(Task{id:"information".into(),title:"신청 정보 준비".into(),detail:"저장된 프로필을 확인하고, 실명·생년월일·상세 주소는 공식 신청서에 정확히 입력해 주세요.".into(),kind:"information"});
        tasks.push(Task {
            id: "official_process".into(),
            title: "공식 신청 절차 확인".into(),
            detail: p
                .application_method
                .clone()
                .filter(|s| !s.trim().is_empty() && s.trim() != "직접입력")
                .unwrap_or_else(|| {
                    "공식 신청처의 본인인증·동의·최종 제출 절차를 확인해 주세요.".into()
                }),
            kind: "handoff",
        });
        if matches!(
            p.program_type.as_str(),
            "loan" | "finance" | "savings" | "financial" | "financial_product"
        ) || p.source_type == "fss_financial"
        {
            tasks.push(Task{id:"financial_terms".into(),title:"금융 상품 조건 확인".into(),detail:"금리·상환·수수료와 약정 내용을 확인해 주세요. 심사 동의와 계약 체결은 금융기관에서 직접 진행해요.".into(),kind:"user_confirmation"});
        }
    }
    // Every identity input of what the user prepares is hashed; a change
    // resets saved progress once. Version 7: resources and destinations are
    // identity only (entry id/url/access, template bytes, destination url),
    // taken from `identity_at`, so verification expiry, re-approval of the
    // same entry and label/explanation wording do not reset a checklist.
    let revision = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({
                "version": 7,
                "resources": stable.resources.revision_material(),
                "official_links": official_links,
                "workflow": stable.workflow,
                "destination": stable.destination.as_ref().map(|d| &d.url),
                "notice": stable.notice_destination.as_ref().map(|d| &d.url),
                "deadline": p.deadline_at,
                "valid_until": p.benefit_valid_until,
                "start": p.application_start_at,
                "end": p.application_end_at,
                "tasks": tasks,
            }))
            .unwrap()
        )
    );
    let selected_channel =
        (policy.workflow.channels.len() == 1).then(|| policy.workflow.channels[0].id.to_owned());
    Ok(Plan {
        revision,
        title: p.title.clone(),
        policy,
        tasks,
        completed_tasks: vec![],
        progress_reset: false,
        selected_channel,
        official_links,
    })
}

pub(crate) async fn load_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<Plan, Error> {
    let mut plan = build_on(connection, id).await?;
    if let Some((revision,completed,selected))=sqlx::query_as::<_,(String,Vec<String>,Option<String>)>("SELECT plan_revision,completed_tasks,selected_channel FROM application_preparations WHERE user_id=$1 AND program_id=$2")
        .bind(user_id).bind(id).fetch_optional(&mut *connection).await.map_err(crate::errors::internal_error)? {
        if revision==plan.revision { plan.completed_tasks=completed; plan.selected_channel=selected.or(plan.selected_channel); } else { plan.progress_reset=true; }
    }
    Ok(plan)
}
pub(crate) async fn save(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
    input: SavePreparation,
) -> Result<Plan, Error> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(crate::errors::internal_error)?;
    save_on(&mut connection, user_id, id, input).await
}

/// [`save`] on the caller's connection, so a confirmed agent action writes
/// in the same transaction that marks its confirmation executed.
pub(crate) async fn save_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    id: Uuid,
    input: SavePreparation,
) -> Result<Plan, Error> {
    let mut plan = build_on(connection, id).await?;
    if input.revision != plan.revision {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"공고 또는 준비 항목이 변경됐어요. 새 목록을 확인해 주세요."})),
        ));
    }
    if input.completed_tasks.len() > plan.tasks.len()
        || input
            .completed_tasks
            .iter()
            .any(|key| !plan.tasks.iter().any(|task| task.id == *key))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"유효하지 않은 준비 항목입니다."})),
        ));
    }
    if let Some(selected) = &input.selected_channel {
        if !plan
            .policy
            .workflow
            .channels
            .iter()
            .any(|c| c.id == selected)
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"공고에 안내된 접수 방식을 선택해 주세요."})),
            ));
        }
        plan.selected_channel = Some(selected.clone());
    } else {
        let saved: Option<String> = sqlx::query_scalar("SELECT selected_channel FROM application_preparations WHERE user_id=$1 AND program_id=$2 AND plan_revision=$3")
            .bind(user_id).bind(id).bind(&plan.revision).fetch_optional(&mut *connection).await.map_err(crate::errors::internal_error)?.flatten();
        plan.selected_channel = saved.or(plan.selected_channel);
    }
    let mut completed = input.completed_tasks;
    completed.sort();
    completed.dedup();
    sqlx::query("INSERT INTO application_preparations(user_id,program_id,plan_revision,completed_tasks,selected_channel) VALUES($1,$2,$3,$4,$5)
         ON CONFLICT(user_id,program_id) DO UPDATE SET plan_revision=$3,completed_tasks=$4,selected_channel=$5,updated_at=now()")
        .bind(user_id).bind(id).bind(&plan.revision).bind(&completed).bind(&plan.selected_channel).execute(&mut *connection).await.map_err(crate::errors::internal_error)?;
    plan.completed_tasks = completed;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::official_links;

    #[test]
    fn official_links_keep_at_most_three_safe_https_urls_with_evidence() {
        let method = "신청 https://provider.example.org/form?id=4#section 접수\n\
                      https://127.0.0.1/secret https://user:pass@example.org/form http://provider.example.org\n\
                      https://provider.example.org/form?id=4#section\n\
                      https://provider.example.org/form?utm_source=mail&id=4 https://provider.example.org/form/?id=4\n\
                      안내 https://a.example.org/, https://b.example.org/guide). https://c.example.org/x https://d.example.org/y";
        let links = official_links(Some(method));
        assert_eq!(links.len(), 3);
        // Stored as written (code review M5); the tracking and trailing-slash
        // variants are the same link by canonical key.
        assert_eq!(
            links[0].url,
            "https://provider.example.org/form?id=4#section"
        );
        assert!(links[0].evidence.starts_with("신청 https://"));
        assert_eq!(links[1].url, "https://a.example.org/");
        assert_eq!(links[2].url, "https://b.example.org/guide");
        let encoded = official_links(Some(
            "접수 https://www.x.go.kr/board/view.do?keyword=%C3%BB%B3%E2&no=3 https://apply.x.or.kr/#/apply/12",
        ));
        assert_eq!(
            encoded.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(),
            [
                "https://www.x.go.kr/board/view.do?keyword=%C3%BB%B3%E2&no=3",
                "https://apply.x.or.kr/#/apply/12"
            ]
        );
        assert!(official_links(None).is_empty());
        assert!(official_links(Some("javascript:alert(1) https://localhost/x")).is_empty());
    }
}
