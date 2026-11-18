//! Grounded, read-only guidance. Topics come from the rule classifier; answers
//! quote stored text (`grounded_qa`). No model supplies facts or actions.
pub mod bundles;
mod eligibility;
pub mod search;
use super::application_preparation::{self, Error, Plan};
use super::{profile_validation::resolve_university, recommendations::ProgramRow};
use crate::assistant_provider::{AnswerMode, Topic};
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use mazimi_core::models::{
    CheckEvidence, EligibilityRule, Evidence, EvidenceCheck, Program, UserProfile,
};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Serialize)]
pub struct Guidance {
    pub version: u8,
    pub program_id: Uuid,
    pub title: String,
    pub amounts: Amounts,
    pub mode: AnswerMode,
    pub ai_available: bool,
    pub notice: &'static str,
    pub eligibility: eligibility::Assessment,
    pub preparation: Plan,
    pub remaining_task_ids: Vec<String>,
    pub alert_enabled: bool,
    pub deadline_at: Option<DateTime<Utc>>,
    pub source: Source,
    pub sections: Vec<Section>,
    /// Grounded answer to a question (POST only): quoted passages, verdict,
    /// not-found. Omitted on the overview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<crate::services::grounded_qa::Answer>,
}

/// Recorded amounts retain their original period; they are never additive.
#[derive(Serialize)]
pub struct Amounts {
    pub monthly: Option<i32>,
    pub semester: Option<i32>,
    pub once: Option<i32>,
}

#[derive(Serialize)]
pub struct Source {
    pub official_url: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub program_updated_at: DateTime<Utc>,
    pub checked_at: DateTime<Utc>,
    pub note: &'static str,
}

#[derive(Serialize)]
pub struct Section {
    pub topic: Topic,
    pub title: &'static str,
    pub body: String,
    pub basis: &'static str,
    /// Citations behind this section (eligibility, documents, deadline).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// 공고 원문 확인일: when the notice was last collected from its source.
    /// Only on sections that restate the notice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_checked_at: Option<DateTime<Utc>>,
}

/// Saved profile plus the catalogue id its school resolves to. Rules compare
/// the catalogue name, exactly as the recommendation preview does.
async fn profile_context(
    connection: &mut sqlx::PgConnection,
    user: Uuid,
) -> Result<(Option<UserProfile>, Option<String>), Error> {
    let mut profile =
        sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_optional(&mut *connection)
            .await
            .map_err(crate::errors::internal_error)?;
    let school = match profile.as_ref().and_then(|p| p.school_name.as_deref()) {
        Some(name) => resolve_university(&mut *connection, name).await?,
        None => None,
    };
    if let (Some(profile), Some((_, name))) = (&mut profile, &school) {
        profile.school_name = Some(name.clone());
    }
    Ok((profile, school.map(|(id, _)| id)))
}

/// Build a consistent read-only snapshot AFTER any optional inference. The
/// caller holds no database connection while waiting on a model.
pub async fn guidance(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
    topics: &[Topic],
    mode: AnswerMode,
    ai_available: bool,
    question: Option<Asked<'_>>,
) -> Result<Guidance, Error> {
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let result = guidance_on(&mut tx, user, id, topics, mode, ai_available, question).await?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(result)
}

pub(super) async fn guidance_on(
    connection: &mut sqlx::PgConnection,
    user: Uuid,
    id: Uuid,
    topics: &[Topic],
    mode: AnswerMode,
    ai_available: bool,
    question: Option<Asked<'_>>,
) -> Result<Guidance, Error> {
    let ProgramRow {
        program,
        university_id,
    } = sqlx::query_as::<_, ProgramRow>(&format!(
        "SELECT * FROM current_programs WHERE id = $1 AND {}",
        mazimi_core::currently_available_sql("", "now()")
    ))
    .bind(id).fetch_optional(&mut *connection).await.map_err(crate::errors::internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error":"현재 안내할 수 있는 혜택을 찾을 수 없어요. 공고 상태를 확인해 주세요."}))))?;
    let rule = sqlx::query_as::<_, EligibilityRule>(
        "SELECT * FROM eligibility_rules WHERE program_id=$1 ORDER BY version DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(crate::errors::internal_error)?;
    let hard_filter = rule.as_ref().and_then(|r| r.hard_filter_json.as_ref());
    let evidence = super::evidence::load(connection, &[(&program, hard_filter)])
        .await
        .map_err(crate::errors::internal_error)?
        .remove(&id)
        .unwrap_or_default();
    let (profile, school) = profile_context(connection, user).await?;
    // A current condition review may widen a code rule; the card and the
    // grounded verdict read the row as the list does.
    let code_overrides = crate::services::condition_reviews::load_overrides(
        &mut *connection,
        Some(std::slice::from_ref(&id)),
    )
    .await
    .map_err(crate::errors::internal_error)?;
    let tier = crate::services::recommendations::stored_tiers(&mut *connection, &[id])
        .await
        .map_err(crate::errors::internal_error)?
        .remove(&id);
    let mut assessment = eligibility::assess_with_overrides(
        &program,
        university_id.as_deref(),
        profile.as_ref(),
        school.as_deref(),
        rule.as_ref(),
        &crate::services::condition_reviews::applied(&code_overrides, &program, hard_filter),
        tier,
    );
    assessment.cite(&evidence);
    let reviewed = crate::services::applicant_reviews::effects(&mut *connection, Some(&[id]))
        .await
        .map_err(crate::errors::internal_error)?;
    if let Some(effect) = reviewed.get(&id) {
        assessment.apply_applicant_review(*effect);
    }
    // The list's own gates decide whether the card may pass (audit H1).
    assessment.apply_list_decision(
        super::recommendations::decide_on(
            &mut *connection,
            &program,
            university_id.as_deref(),
            profile.as_ref(),
            school.as_deref(),
            rule.as_ref(),
            reviewed.get(&id).copied(),
        )
        .await
        .map_err(crate::errors::internal_error)?,
    );
    let answer = question.map(|asked| {
        crate::services::grounded_qa::answer(&crate::services::grounded_qa::Inputs {
            question: asked.question,
            previous_topic: asked.previous_topic,
            program: &program,
            program_university_id: university_id.as_deref(),
            // The same facts `eligibility::assess` compares.
            facts: mazimi_core::structural::StructuralProfile {
                birth_year: profile.as_ref().and_then(|p| p.birth_year),
                birth_date: profile.as_ref().and_then(|p| p.birth_date),
                region_code: profile.as_ref().and_then(|p| p.region_code.as_deref()),
                enrollment_status: profile
                    .as_ref()
                    .and_then(|p| p.enrollment_status.as_deref()),
                university_id: school.as_deref(),
                city_code: profile.as_ref().and_then(|p| p.city_code.as_deref()),
            },
            overall: assessment.overall(),
            evidence: &evidence,
        })
    });
    let plan = application_preparation::load_on(connection, user, id).await?;
    let alert_enabled = sqlx::query_scalar::<_, bool>(
        "SELECT enabled FROM alert_subscriptions WHERE user_id=$1 AND program_id=$2",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(crate::errors::internal_error)?
    .unwrap_or(false);
    let remaining_task_ids = plan
        .tasks
        .iter()
        .filter(|t| !plan.completed_tasks.contains(&t.id))
        .map(|t| t.id.clone())
        .collect();
    let deadline = program.effective_deadline();
    let source = Source {
        official_url: safe_official_url(program.official_url.as_deref()),
        last_synced_at: program.last_synced_at, program_updated_at:program.updated_at, checked_at:Utc::now(),
        note:"저장된 공식 출처 자료를 기준으로 안내해요. 수집 시각은 지금 원문을 재검증했다는 뜻이 아니에요. 최종 조건은 공식 공고를 확인해 주세요.",
    };
    let sections = topics
        .iter()
        .map(|topic| {
            section(
                *topic,
                &program,
                &plan,
                &assessment,
                alert_enabled,
                deadline,
                &evidence,
            )
        })
        .collect();
    // A corporate offer's amounts are no support money (pays_support).
    let paid = mazimi_core::pays_support(&program.program_type);
    Ok(Guidance { version:1, program_id:id, title:program.title,
        amounts: Amounts { monthly:program.benefit_amount_monthly.filter(|_| paid), semester:program.benefit_amount_semester.filter(|_| paid), once:program.benefit_amount_once.filter(|_| paid) }, mode, ai_available,
        notice:"답변은 현재 저장된 프로필을 기준으로 해요. 질문에 적은 조건으로 프로필·알림·신청 상태를 변경하지 않아요.",
        eligibility:assessment, preparation:plan, remaining_task_ids, alert_enabled, deadline_at:deadline, source, sections, answer })
}

/// A question for the grounded answer, with the previous answer's topic for
/// follow-ups.
#[derive(Clone, Copy)]
pub struct Asked<'a> {
    pub question: &'a str,
    pub previous_topic: Option<Topic>,
}

pub(crate) fn safe_official_url(value: Option<&str>) -> Option<String> {
    mazimi_core::source_policy::canonicalize_public_https_url(value?)
        .ok()
        .map(|url| url.canonical_url)
}

fn section(
    topic: Topic,
    p: &Program,
    plan: &Plan,
    assessment: &eligibility::Assessment,
    alert: bool,
    deadline: Option<DateTime<Utc>>,
    evidence: &[CheckEvidence],
) -> Section {
    let (title, body, basis) = match topic {
        Topic::Summary => ("혜택 한눈에 보기", p.summary.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "확인된 요약이 아직 없어요. 공식 안내에서 지원 내용을 확인해 주세요.".into()), "program_record"),
        Topic::Eligibility => ("나의 조건 확인", assessment.summary.to_owned(), "saved_profile_and_rules"),
        Topic::Documents => {
            // Registered document rows only (the preparation's `document:*`
            // tasks). Free-text mentions are not turned into document names.
            let documents: Vec<_> = plan.tasks.iter().filter(|t| t.kind == "document").map(|t| t.title.clone()).collect();
            ("준비할 서류", if documents.is_empty() { "확인된 서류 목록이 없어요. 서류가 필요 없다는 뜻은 아니에요. 공식 신청처에서 확인해 주세요.".into() }
                else { format!("공고에서 확인한 서류: {}. 필수·선택 여부와 발급일·제출 형식은 신청 준비 화면에서 확인해 주세요.", documents.join(" · ")) }, "program_record")
        },
        Topic::Deadline => ("마감 확인", match deadline {
            Some(date) => format!("등록된 가장 빠른 마감은 {} (한국 시간)이에요.",date.with_timezone(&chrono::FixedOffset::east_opt(9*3600).unwrap()).format("%Y-%m-%d %H:%M")),
            None => "확인된 마감 시각이 없어요. 공식 공고의 기간을 확인해 주세요.".into(),
        }, "program_record"),
        Topic::Steps => {
            let remaining: Vec<_> = plan.tasks.iter().filter(|t| !plan.completed_tasks.contains(&t.id)).collect();
            ("다음에 할 일", if let Some(next) = remaining.first() {
                format!("체크리스트에서 아직 확인하지 않은 항목이 {}개예요. 다음 항목은 ‘{}’이에요. {}", remaining.len(),next.title,next.detail)
            } else { "체크리스트를 모두 확인했어요. 공식 절차와 접수 결과를 확인해 주세요.".into() }, "user_preparation")
        },
        Topic::Alerts => ("혜택 알림", if alert { "이 혜택의 알림 구독이 켜져 있어요.".into() }
            else { "이 혜택의 알림 구독이 꺼져 있어요. 아래 ‘이 혜택 알림 켜기’를 직접 눌러 설정할 수 있어요.".into() }, "app_settings"),
        Topic::Unsupported => ("확인 가능한 범위", "현재는 이 혜택의 요약·저장된 프로필과 조건 비교·준비 서류·마감·신청 절차·알림을 안내해요.".into(), "app_policy"),
    };
    let cited = match topic {
        Topic::Eligibility => assessment
            .checks
            .iter()
            .flat_map(|c| c.evidence.iter().cloned())
            .collect(),
        Topic::Documents => super::evidence::for_check(evidence, EvidenceCheck::Documents),
        Topic::Deadline => super::evidence::for_check(evidence, EvidenceCheck::ApplicationWindow),
        _ => Vec::new(),
    };
    Section {
        topic,
        title,
        body,
        basis,
        evidence: cited,
        source_checked_at: matches!(
            topic,
            Topic::Eligibility | Topic::Documents | Topic::Deadline
        )
        .then_some(p.last_synced_at)
        .flatten(),
    }
}

#[derive(Serialize)]
pub struct Briefing {
    pub scope: &'static str,
    pub items: Vec<BriefingItem>,
    pub generated_at: DateTime<Utc>,
}
#[derive(Serialize)]
pub struct BriefingItem {
    pub program_id: Uuid,
    pub title: String,
    pub deadline_at: Option<DateTime<Utc>>,
    pub reason: &'static str,
    pub next_task: String,
    pub remaining_count: usize,
    pub progress_reset: bool,
}

/// Bounded shortlist: user's saved/started/subscribed programmes only, not a
/// claim to have searched all benefits. No LLM or scheduled writes on page load.
/// One read-only snapshot: the shortlist, rules and profile are loaded once;
/// only the per-programme preparation plan is built per item (at most 3).
pub async fn briefing(pool: &PgPool, user: Uuid) -> Result<Briefing, Error> {
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let rows = sqlx::query_as::<_, ProgramRow>(&format!(
        "SELECT p.* FROM current_programs p WHERE {available}
         AND (EXISTS(SELECT 1 FROM user_bookmarks b WHERE b.user_id=$1 AND b.program_id=p.id)
           OR EXISTS(SELECT 1 FROM application_preparations a WHERE a.user_id=$1 AND a.program_id=p.id)
           OR EXISTS(SELECT 1 FROM alert_subscriptions a WHERE a.user_id=$1 AND a.program_id=p.id AND a.enabled))
         AND NOT EXISTS(SELECT 1 FROM application_journeys j WHERE j.user_id=$1 AND j.program_id=p.id AND j.outcome='reported_applied')
         ORDER BY LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) ASC NULLS LAST,p.id LIMIT 3",
        available = mazimi_core::currently_available_sql("p.", "now()")
    ))
    .bind(user)
    .fetch_all(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.program.id).collect();
    let mut rules: std::collections::HashMap<Uuid, EligibilityRule> =
        sqlx::query_as::<_, EligibilityRule>(
            "SELECT DISTINCT ON (program_id) * FROM eligibility_rules
             WHERE program_id = ANY($1) ORDER BY program_id, version DESC",
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?
        .into_iter()
        .map(|r| (r.program_id, r))
        .collect();
    let (profile, school) = if rows.is_empty() {
        (None, None)
    } else {
        profile_context(&mut tx, user).await?
    };
    let reviewed = crate::services::applicant_reviews::effects(&mut tx, Some(&ids))
        .await
        .map_err(crate::errors::internal_error)?;
    let tiers = crate::services::recommendations::stored_tiers(&mut tx, &ids)
        .await
        .map_err(crate::errors::internal_error)?;
    let code_overrides = crate::services::condition_reviews::load_overrides(&mut tx, Some(&ids))
        .await
        .map_err(crate::errors::internal_error)?;
    let now = Utc::now();
    let mut items = Vec::new();
    for ProgramRow {
        program,
        university_id,
    } in rows
    {
        let rule = rules.remove(&program.id);
        let applied = crate::services::condition_reviews::applied(
            &code_overrides,
            &program,
            rule.as_ref().and_then(|r| r.hard_filter_json.as_ref()),
        );
        let mut assessment = eligibility::assess_with_overrides(
            &program,
            university_id.as_deref(),
            profile.as_ref(),
            school.as_deref(),
            rule.as_ref(),
            &applied,
            tiers.get(&program.id).copied(),
        );
        if let Some(effect) = reviewed.get(&program.id) {
            assessment.apply_applicant_review(*effect);
        }
        assessment.apply_list_decision(
            super::recommendations::decide_on(
                &mut tx,
                &program,
                university_id.as_deref(),
                profile.as_ref(),
                school.as_deref(),
                rule.as_ref(),
                reviewed.get(&program.id).copied(),
            )
            .await
            .map_err(crate::errors::internal_error)?,
        );
        let plan = application_preparation::load_on(&mut tx, user, program.id).await?;
        let remaining: Vec<_> = plan
            .tasks
            .iter()
            .filter(|t| !plan.completed_tasks.contains(&t.id))
            .collect();
        let fail = assessment.status == mazimi_core::rule_engine::RuleVerdict::Fail;
        let deadline_at = program.effective_deadline();
        items.push(BriefingItem {
            program_id: program.id,
            title: program.title,
            deadline_at,
            reason: if fail {
                "현재 조건 불일치 확인"
            } else if plan.progress_reset {
                "공고 변경으로 다시 확인"
            } else if deadline_at.is_some_and(|d| d <= now + chrono::Duration::days(7)) {
                "7일 이내 마감"
            } else {
                "저장한 혜택 이어서 준비"
            },
            next_task: if fail {
                "저장한 프로필과 공식 대상 조건 비교".into()
            } else {
                remaining
                    .first()
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| "공식 접수 결과 확인".into())
            },
            remaining_count: remaining.len(),
            progress_reset: plan.progress_reset,
        });
    }
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Briefing {
        scope: "saved_programs_deadline_first",
        items,
        generated_at: now,
    })
}
