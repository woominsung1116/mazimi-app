//! Write tools as confirmations (spec §8).
//!
//! A plan step that changes state never executes. [`propose`] inspects the
//! current state, and either reports that nothing needs to change or stores
//! a confirmation bound by [`confirmation_hash`] to the user, run, tool call,
//! normalized arguments, observed precondition and expiry. [`approve`]
//! executes it once: the client must present the hash it displayed, the row
//! must be pending and unexpired, the stored hash must still match its own
//! fields, and the state observed now must equal the precondition (else
//! `409 stale_confirmation`). The write runs in the transaction that marks
//! the confirmation executed; a second approval replays the stored result.
use super::{
    store::{self, error, ConfirmationRow, Error},
    tools::tool_error,
    ConfirmationPreview, ConfirmationView, ToolError,
};
use crate::services::application_preparation::{self, SavePreparation};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use mazimi_core::agent::{
    confirmation_hash, ApplicationStatus, CalendarDate, ConfirmationBinding, ProfileField,
    RunStatus, ToolCall, ToolName,
};
use mazimi_core::models::{ProfileInput, UserProfile};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// What the form helper fills before the user types: exactly the device's
/// suggestions (`apps/mobile/lib/formFill/fields.ts` `suggest`), each a
/// proposal the user confirms (QA B15: the copy promised a profile prefill
/// the production forms never got). The one statement of it: the form card's
/// 승인하면 cell, which the post-approval message repeats from the card.
pub(crate) const FORM_PREFILL: &str = "있는 칸만 미리 채움(이 기기에 기억한 이름·연락처·주소·이메일, 프로필의 학교·학년, 작성일), 나머지 칸과 제출은 직접";

/// Normalized arguments of a write, as stored and hashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WriteArgs {
    Bookmark {
        bookmarked: bool,
    },
    Alert {
        enabled: bool,
    },
    Checklist {
        task_id: String,
        done: bool,
    },
    Status {
        status: ApplicationStatus,
    },
    /// One whitelisted field of the user's own profile.
    Profile {
        field: ProfileField,
        value: String,
    },
    /// Open the form helper with a draft built on the device.
    Form {
        prefill: bool,
    },
    /// Open the form helper at its signature step. The signature is drawn
    /// and kept on the device; the server never receives it.
    Signature {
        signature_on_device: bool,
    },
    /// Open the official application page the resolver verified. Bound to
    /// that URL; nothing is submitted.
    Handoff {
        official_destination: String,
    },
    /// An all-day event the device writes to its own calendar. The date is
    /// the server's reading of the programme ([`super::calendar`]).
    Calendar {
        calendar_date: CalendarDate,
        date: chrono::NaiveDate,
        /// The event sits this many days before `calendar_date` (0 = on it).
        #[serde(default, skip_serializing_if = "is_zero")]
        days_before: u8,
    },
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// "신청 마감" or "신청 마감 1일 전".
fn calendar_label(kind: CalendarDate, days_before: u8) -> String {
    match days_before {
        0 => kind.label().to_owned(),
        n => format!("{} {n}일 전", kind.label()),
    }
}

/// The programme id a write that names none is bound to (`profile_patch`).
pub const NO_PROGRAM: Uuid = Uuid::nil();

/// State observed for a write: what the card shows and what execution must
/// find unchanged.
pub struct Inspection {
    pub args: WriteArgs,
    pub precondition: Value,
    pub preview: ConfirmationPreview,
    /// Already in the requested state: nothing to confirm.
    pub no_op: bool,
}

fn unavailable() -> ToolError {
    tool_error(
        "program_unavailable",
        "지금 안내할 수 있는 혜택이 아니에요. 공고 상태를 확인해 주세요.",
    )
}

fn db_error(error: sqlx::Error) -> ToolError {
    tracing::warn!(error = %error, "agent write inspection failed");
    tool_error("tool_failed", "현재 상태를 확인하지 못했어요.")
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A checklist task named by id or by a fragment of its title. Exactly one
/// match, or the shortest title when one title contains the fragment
/// exactly; otherwise no proposal.
pub fn match_task<'a>(
    tasks: &'a [application_preparation::Task],
    wanted: &str,
) -> Result<&'a application_preparation::Task, ToolError> {
    if let Some(task) = tasks.iter().find(|t| t.id == wanted) {
        return Ok(task);
    }
    let key = compact(wanted);
    if key.chars().count() < 2 {
        return Err(tool_error(
            "task_not_found",
            "체크할 준비 항목을 찾지 못했어요.",
        ));
    }
    let mut matches: Vec<&application_preparation::Task> = tasks
        .iter()
        .filter(|t| {
            let title = compact(&t.title);
            title.contains(&key) || key.contains(&title)
        })
        .collect();
    matches.sort_by_key(|t| t.title.chars().count());
    match matches.as_slice() {
        [] => Err(tool_error(
            "task_not_found",
            "체크할 준비 항목을 찾지 못했어요. 신청 준비 화면에서 직접 확인해 주세요.",
        )),
        [one] => Ok(one),
        [first, second, ..] if compact(&first.title) == key && compact(&second.title) != key => {
            Ok(first)
        }
        _ => Err(tool_error(
            "ambiguous_task",
            "비슷한 준비 항목이 여러 개예요. 신청 준비 화면에서 직접 골라 주세요.",
        )),
    }
}

/// Status edges a user may record. Settled outcomes do not move backwards.
pub fn status_transition_allowed(from: Option<&str>, to: ApplicationStatus) -> bool {
    use ApplicationStatus::*;
    match from {
        None | Some("interested" | "planning" | "applying") => true,
        Some("applied") => matches!(to, Waiting | Received | Abandoned),
        Some("waiting") => matches!(to, Received | Abandoned),
        Some("received") => false,
        Some("abandoned") => matches!(to, Interested | Planning | Applying),
        Some(_) => false,
    }
}

async fn available_title(
    connection: &mut PgConnection,
    program: Uuid,
) -> Result<String, ToolError> {
    sqlx::query_scalar::<_, String>(&format!(
        "SELECT title FROM current_programs WHERE id=$1 AND {}",
        mazimi_core::currently_available_sql("", "now()")
    ))
    .bind(program)
    .fetch_optional(connection)
    .await
    .map_err(db_error)?
    .ok_or_else(unavailable)
}

/// Normalize a planned write against the programme it resolved to.
pub async fn normalize(
    connection: &mut PgConnection,
    user: Uuid,
    program: Uuid,
    call: &ToolCall,
) -> Result<WriteArgs, ToolError> {
    Ok(match call {
        ToolCall::Bookmark(a) => WriteArgs::Bookmark {
            bookmarked: a.bookmarked,
        },
        ToolCall::SetAlert(a) => WriteArgs::Alert { enabled: a.enabled },
        ToolCall::ToggleChecklistItem(a) => {
            let plan = application_preparation::load_on(connection, user, program)
                .await
                .map_err(|_| unavailable())?;
            let task = match_task(&plan.tasks, &a.task)?;
            WriteArgs::Checklist {
                task_id: task.id.clone(),
                done: a.done,
            }
        }
        ToolCall::RecordApplicationStatus(a) => WriteArgs::Status { status: a.status },
        ToolCall::ProfilePatch(a) => {
            // Validated by the plan; checked again so a stored row can
            // never carry a value outside the field's list.
            if !a.field.accepts(&a.value) {
                return Err(tool_error("invalid_value", "바꿀 값을 확인하지 못했어요."));
            }
            WriteArgs::Profile {
                field: a.field,
                value: a.value.clone(),
            }
        }
        ToolCall::OpenFormFill(_) => WriteArgs::Form { prefill: true },
        ToolCall::RequestSignature(_) => WriteArgs::Signature {
            signature_on_device: true,
        },
        ToolCall::OpenOfficialApplication(_) => WriteArgs::Handoff {
            official_destination: official_destination(connection, user, program).await?,
        },
        ToolCall::AddToCalendar(a) => {
            let proposal =
                super::calendar::resolve(connection, program, a.date, a.days_before).await?;
            WriteArgs::Calendar {
                calendar_date: a.date,
                date: proposal.date,
                days_before: a.days_before,
            }
        }
        _ => return Err(tool_error("not_a_write", "변경 도구가 아니에요.")),
    })
}

/// Observe the state a write depends on. Called when proposing and again,
/// inside the executing transaction, before applying it.
pub async fn inspect(
    connection: &mut PgConnection,
    user: Uuid,
    program: Uuid,
    args: &WriteArgs,
) -> Result<Inspection, ToolError> {
    match args {
        WriteArgs::Bookmark { bookmarked } => {
            let title = available_title(connection, program).await?;
            let current: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM user_bookmarks WHERE user_id=$1 AND program_id=$2)",
            )
            .bind(user)
            .bind(program)
            .fetch_one(&mut *connection)
            .await
            .map_err(db_error)?;
            let label = |on: bool| {
                if on {
                    "관심 혜택에 저장됨"
                } else {
                    "저장 안 됨"
                }
            };
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"bookmarked": current}),
                no_op: current == *bookmarked,
                preview: ConfirmationPreview {
                    title,
                    action: if *bookmarked {
                        "관심 혜택에 저장".into()
                    } else {
                        "관심 혜택에서 빼기".into()
                    },
                    before: label(current).into(),
                    after: label(*bookmarked).into(),
                    reversible: true,
                    note: None,
                },
            })
        }
        WriteArgs::Alert { enabled } => {
            // A 마감 알림 is about a deadline: none, nothing to alert on (QA
            // B5). Turning one off is always possible, on a closed or undated
            // programme too (verifier).
            let title = if *enabled {
                available_title(connection, program).await?
            } else {
                sqlx::query_scalar::<_, String>("SELECT title FROM programs WHERE id=$1")
                    .bind(program)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(db_error)?
                    .ok_or_else(unavailable)?
            };
            if *enabled {
                let dated: bool = sqlx::query_scalar(
                    "SELECT LEAST(deadline_at, application_end_at, benefit_valid_until) IS NOT NULL
                     FROM programs WHERE id=$1",
                )
                .bind(program)
                .fetch_one(&mut *connection)
                .await
                .map_err(db_error)?;
                if !dated {
                    return Err(tool_error(
                        "no_deadline",
                        "마감일 정보가 없어 마감 알림을 켤 수 없어요.",
                    ));
                }
            }
            let current: bool = sqlx::query_scalar::<_, bool>(
                "SELECT enabled FROM alert_subscriptions WHERE user_id=$1 AND program_id=$2",
            )
            .bind(user)
            .bind(program)
            .fetch_optional(&mut *connection)
            .await
            .map_err(db_error)?
            .unwrap_or(false);
            let label = |on: bool| if on { "알림 켜짐" } else { "알림 꺼짐" };
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"enabled": current}),
                no_op: current == *enabled,
                preview: ConfirmationPreview {
                    title,
                    action: if *enabled {
                        "이 혜택의 마감 알림 켜기".into()
                    } else {
                        "이 혜택의 마감 알림 끄기".into()
                    },
                    before: label(current).into(),
                    after: label(*enabled).into(),
                    reversible: true,
                    note: None,
                },
            })
        }
        WriteArgs::Checklist { task_id, done } => {
            let plan = application_preparation::load_on(connection, user, program)
                .await
                .map_err(|_| unavailable())?;
            let task = plan
                .tasks
                .iter()
                .find(|t| t.id == *task_id)
                .ok_or_else(|| tool_error("task_not_found", "준비 항목이 바뀌었어요."))?;
            let current = plan.completed_tasks.contains(task_id);
            let label = |on: bool| if on { "완료" } else { "미완료" };
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"revision": plan.revision, "done": current}),
                no_op: current == *done,
                preview: ConfirmationPreview {
                    title: plan.title.clone(),
                    action: format!(
                        "준비 항목 ‘{}’ {}",
                        task.title,
                        if *done {
                            "완료로 표시"
                        } else {
                            "완료 표시 해제"
                        }
                    ),
                    before: label(current).into(),
                    after: label(*done).into(),
                    reversible: true,
                    note: None,
                },
            })
        }
        WriteArgs::Status { status } => {
            let title: String = sqlx::query_scalar("SELECT title FROM programs WHERE id=$1")
                .bind(program)
                .fetch_optional(&mut *connection)
                .await
                .map_err(db_error)?
                .ok_or_else(unavailable)?;
            let current: Option<String> = sqlx::query_scalar(
                "SELECT state FROM user_program_states WHERE user_id=$1 AND program_id=$2",
            )
            .bind(user)
            .bind(program)
            .fetch_optional(&mut *connection)
            .await
            .map_err(db_error)?;
            if current.as_deref() != Some(status.as_str())
                && !status_transition_allowed(current.as_deref(), *status)
            {
                return Err(tool_error(
                    "invalid_transition",
                    "이미 기록한 결과에서 이 상태로는 바꿀 수 없어요. 신청 관리 화면에서 확인해 주세요.",
                ));
            }
            let before = current
                .as_deref()
                .map(|s| crate::routes::state::display_status(s, None).1.to_owned())
                .unwrap_or_else(|| "기록 없음".into());
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"state": current}),
                no_op: current.as_deref() == Some(status.as_str()),
                preview: ConfirmationPreview {
                    title,
                    action: format!("신청 상태를 ‘{}’(으)로 직접 기록", status.label()),
                    before,
                    after: status.label().into(),
                    reversible: !matches!(status, ApplicationStatus::Received),
                    note: None,
                },
            })
        }
        WriteArgs::Profile { field, value } => {
            // The column is a constant of the closed field list.
            // FOR UPDATE: inside `approve` the row stays locked until the
            // write commits, so a concurrent profile save cannot land between
            // this check and the update (security review L1).
            let row: Option<(Option<String>, i32, Option<String>)> = sqlx::query_as(&format!(
                "SELECT {}::text, profile_version, city_code FROM user_profiles WHERE user_id=$1 FOR UPDATE",
                field.as_str()
            ))
            .bind(user)
            .fetch_optional(&mut *connection)
            .await
            .map_err(db_error)?;
            let (current, version, district) = row.ok_or_else(|| {
                tool_error(
                    "profile_missing",
                    "아직 저장한 프로필이 없어요. 마이페이지에서 기본 정보를 먼저 저장해 주세요.",
                )
            })?;
            // The whole row must still pass the profile rules with this one
            // change; otherwise no card (code review: never fail only after
            // approval).
            let full =
                sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id=$1")
                    .bind(user)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(db_error)?;
            if patched_profile(&full, *field, value).is_err() {
                return Err(tool_error(
                    "profile_invalid",
                    "저장된 프로필에 확인이 필요한 항목이 있어요. 마이페이지에서 프로필을 먼저 확인해 주세요.",
                ));
            }
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"field": field, "value": current, "profile_version": version}),
                no_op: current.as_deref() == Some(value.as_str()),
                preview: ConfirmationPreview {
                    title: "내 프로필".into(),
                    action: format!("프로필의 ‘{}’ 바꾸기", field.label()),
                    before: field.value_label(current.as_deref()),
                    // A district belongs to its region: say it goes too.
                    after: if *field == ProfileField::RegionCode
                        && district.is_some()
                        && current.as_deref() != Some(value.as_str())
                    {
                        format!(
                            "{} · 시·군·구 선택은 지워져요",
                            field.value_label(Some(value))
                        )
                    } else {
                        field.value_label(Some(value))
                    },
                    reversible: true,
                    note: Some("마이페이지에서 언제든 다시 고칠 수 있어요.".into()),
                },
            })
        }
        WriteArgs::Calendar {
            calendar_date,
            date,
            days_before,
        } => {
            // The date is read again: a changed deadline makes the card stale.
            let proposal =
                super::calendar::resolve(connection, program, *calendar_date, *days_before).await?;
            let title: String = sqlx::query_scalar("SELECT title FROM programs WHERE id=$1")
                .bind(program)
                .fetch_one(&mut *connection)
                .await
                .map_err(db_error)?;
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({
                    "date": proposal.date,
                    "event_title": proposal.event_title,
                    "quote": proposal.quote,
                    "source_label": proposal.source_label,
                }),
                no_op: false,
                preview: ConfirmationPreview {
                    title,
                    action: format!(
                        "이 기기 캘린더에 ‘{}’ 일정 추가 ({})",
                        calendar_label(*calendar_date, *days_before),
                        proposal.date.format("%Y. %-m. %-d.")
                    ),
                    before: "캘린더에 추가 전".into(),
                    after: if proposal.date == *date {
                        format!(
                            "{} 하루 종일 일정 · 근거: {}",
                            proposal.date.format("%Y. %-m. %-d."),
                            proposal.quote
                        )
                    } else {
                        "날짜가 바뀌었어요".into()
                    },
                    reversible: true,
                    // The action and 승인하면 say what goes in; this is what
                    // the device does around it.
                    note: Some(
                        "처음이면 캘린더 권한을 물어봐요. 일정은 서버에 저장하지 않아요.".into(),
                    ),
                },
            })
        }
        WriteArgs::Signature { .. } => {
            let title = available_title(connection, program).await?;
            let templates = form_templates(connection, user, program).await?;
            if templates.is_empty() {
                return Err(tool_error(
                    "no_form_template",
                    "서명할 신청서 초안이 없어 서명 단계는 건너뛰었어요. 서명은 공식 신청처에서 직접 해 주세요.",
                ));
            }
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"templates": templates
                    .iter()
                    .map(|t| json!({"id": t.id, "content_hash": t.content_hash}))
                    .collect::<Vec<_>>()}),
                no_op: false,
                preview: ConfirmationPreview {
                    title,
                    action: "작성 도우미의 서명 단계 열기 (서명은 이 기기에서만)".into(),
                    before: "서명 전".into(),
                    after: "초안에 이 기기에서 직접 서명 · 서버로 보내지 않음 · 제출은 직접".into(),
                    reversible: true,
                    note: None,
                },
            })
        }
        WriteArgs::Handoff {
            official_destination: url,
        } => {
            let title = available_title(connection, program).await?;
            let current = official_destination(connection, user, program).await?;
            // The card opens the URL it names, never a newer one (security
            // review L2).
            if current != *url {
                return Err(tool_error(
                    "destination_changed",
                    "신청 페이지 주소가 바뀌었어요. 다시 요청해 주세요.",
                ));
            }
            let host = current
                .strip_prefix("https://")
                .and_then(|rest| rest.split(['/', '?', '#']).next())
                .unwrap_or_default()
                .to_owned();
            Ok(Inspection {
                args: args.clone(),
                // A changed destination makes the card stale.
                precondition: json!({"official_destination": current}),
                no_op: false,
                preview: ConfirmationPreview {
                    title,
                    action: format!("공식 신청 페이지({host}) 열기"),
                    before: "마지미".into(),
                    after: "공식 사이트에서 직접 로그인·제출 (마지미는 제출하지 않음)".into(),
                    reversible: true,
                    note: None,
                },
            })
        }
        WriteArgs::Form { .. } => {
            let title = available_title(connection, program).await?;
            let templates = form_templates(connection, user, program).await?;
            if templates.is_empty() {
                return Err(tool_error(
                    "no_form_template",
                    "이 혜택은 앱에서 채울 수 있는 서식이 아직 없어요. 복사용 안내와 공식 양식을 확인해 주세요.",
                ));
            }
            Ok(Inspection {
                args: args.clone(),
                precondition: json!({"templates": templates
                    .iter()
                    .map(|t| json!({"id": t.id, "content_hash": t.content_hash}))
                    .collect::<Vec<_>>()}),
                no_op: false,
                preview: ConfirmationPreview {
                    title,
                    action: "서식 작성 도우미를 열고 이 기기에서 초안 채우기".into(),
                    before: "작성 전".into(),
                    after: FORM_PREFILL.into(),
                    reversible: true,
                    note: None,
                },
            })
        }
    }
}

/// The verified official application page of a programme (the resolver's
/// destination: a public HTTPS URL), or no card.
async fn official_destination(
    connection: &mut PgConnection,
    user: Uuid,
    program: Uuid,
) -> Result<String, ToolError> {
    let plan = application_preparation::load_on(connection, user, program)
        .await
        .map_err(|_| unavailable())?;
    // Only a reviewed application resource is the official application page
    // (security review M2): a notice link or a crawled `official_url` gets
    // no card. The raw URL is the detail page CTA's (it may carry an SPA
    // route); it must still pass the public-https policy.
    plan.policy
        .destination
        .filter(|d| {
            d.evidence == "reviewed_application"
                && matches!(d.kind, "application" | "login_required")
        })
        .map(|d| d.url)
        .filter(|url| crate::services::benefit_assistant::safe_official_url(Some(url)).is_some())
        .ok_or_else(|| {
            tool_error(
                "no_official_destination",
                "확인된 공식 신청 페이지가 아직 없어요. 공고문의 신청 방법을 확인해 주세요.",
            )
        })
}

/// Templates the form helper can open for this programme, in id order.
async fn form_templates(
    connection: &mut PgConnection,
    user: Uuid,
    program: Uuid,
) -> Result<Vec<crate::services::application_resources::PublicResource>, ToolError> {
    let plan = application_preparation::load_on(connection, user, program)
        .await
        .map_err(|_| unavailable())?;
    let mut templates = plan.policy.resources.templates;
    templates.sort_by_key(|t| t.id);
    Ok(templates)
}

/// The draft profile after one whitelisted change, validated by the same
/// rules as `POST /api/v1/profile`.
fn patched_profile(
    row: &UserProfile,
    field: ProfileField,
    value: &str,
) -> Result<ProfileInput, Error> {
    let invalid = || {
        error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_profile",
            "프로필을 바꿀 수 없어요. 마이페이지에서 확인해 주세요.",
        )
    };
    // The one saved-profile conversion the list and the card read (arch L2).
    let mut input = crate::services::benefit_assistant::bundles::profile_input(row.clone())
        .ok_or_else(invalid)?;
    match field {
        ProfileField::RegionCode => input.region_code = value.to_owned(),
        ProfileField::EnrollmentStatus => input.enrollment_status = Some(value.to_owned()),
        ProfileField::EmploymentStatus => input.employment_status = Some(value.to_owned()),
        ProfileField::SchoolYear => input.school_year = field.number(value),
        ProfileField::IncomeBracket => input.income_bracket = field.number(value),
        ProfileField::KosafSupportBracket => input.kosaf_support_bracket = field.number(value),
        ProfileField::HouseholdSize => input.household_size = field.number(value),
    }
    if !field.accepts(value) {
        return Err(invalid());
    }
    crate::services::profile_validation::normalize_profile(&mut input).map_err(|_| invalid())?;
    Ok(input)
}

pub struct Proposed {
    pub confirmation: Option<ConfirmationView>,
    pub preview: ConfirmationPreview,
    pub args: WriteArgs,
}

/// Inspect a write and, unless it is a no-op, store its confirmation.
#[allow(clippy::too_many_arguments)]
pub async fn propose(
    pool: &PgPool,
    user: Uuid,
    run: Uuid,
    tool_call: Uuid,
    tool: ToolName,
    program: Uuid,
    call: &ToolCall,
    ttl: std::time::Duration,
) -> Result<Proposed, ToolError> {
    let mut connection = pool.acquire().await.map_err(db_error)?;
    let args = normalize(&mut connection, user, program, call).await?;
    let inspection = inspect(&mut connection, user, program, &args).await?;
    if inspection.no_op {
        return Ok(Proposed {
            confirmation: None,
            preview: inspection.preview,
            args,
        });
    }
    let arguments = serde_json::to_value(&args)
        .map_err(|_| tool_error("tool_failed", "변경 내용을 만들지 못했어요."))?;
    // Second precision: the hash binds the timestamp in whole seconds.
    let expires_at: DateTime<Utc> = DateTime::from_timestamp(
        (Utc::now() + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::minutes(10)))
            .timestamp(),
        0,
    )
    .unwrap_or_else(Utc::now);
    let payload_hash = confirmation_hash(&ConfirmationBinding {
        user_id: user,
        run_id: run,
        tool_call_id: tool_call,
        tool,
        program_id: program,
        arguments: &arguments,
        precondition: &inspection.precondition,
        expires_at,
    });
    let preview = serde_json::to_value(&inspection.preview).unwrap_or(Value::Null);
    let row = sqlx::query_as::<_, ConfirmationRow>(
        "INSERT INTO assistant_confirmations(run_id, user_id, tool_call_id, tool_name, program_id,
            arguments, precondition, preview, payload_hash, expires_at)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10
         -- A run cancelled meanwhile gets no new card.
         WHERE EXISTS(SELECT 1 FROM assistant_runs WHERE id=$1 AND status='tool_running')
         RETURNING id, run_id, user_id, tool_call_id, tool_name, program_id, arguments, precondition,
                   preview, payload_hash, status, expires_at, result",
    )
    .bind(run)
    .bind(user)
    .bind(tool_call)
    .bind(tool.as_str())
    .bind(program)
    .bind(&arguments)
    .bind(&inspection.precondition)
    .bind(&preview)
    .bind(&payload_hash)
    .bind(expires_at)
    .fetch_optional(&mut *connection)
    .await
    .map_err(db_error)?
    .ok_or_else(|| tool_error("run_closed", "요청이 취소돼 변경을 제안하지 않았어요."))?;
    Ok(Proposed {
        confirmation: Some(row.view()),
        preview: inspection.preview,
        args,
    })
}

/// Apply a confirmed write on the executing transaction.
async fn apply(
    connection: &mut PgConnection,
    user: Uuid,
    program: Uuid,
    args: &WriteArgs,
) -> Result<Value, Error> {
    match args {
        WriteArgs::Bookmark { bookmarked: true } => {
            // The route's statement (routes/bookmark.rs `write`): the write
            // itself admits only a published programme (review M9).
            sqlx::query(
                "INSERT INTO user_bookmarks (user_id, program_id) \
                 SELECT $1, id FROM current_programs WHERE id = $2 \
                 ON CONFLICT (user_id, program_id) DO NOTHING",
            )
            .bind(user)
            .bind(program)
            .execute(&mut *connection)
            .await
            .map_err(store::db)?;
            Ok(json!({"bookmarked": true}))
        }
        WriteArgs::Bookmark { bookmarked: false } => {
            sqlx::query("DELETE FROM user_bookmarks WHERE user_id=$1 AND program_id=$2")
                .bind(user)
                .bind(program)
                .execute(&mut *connection)
                .await
                .map_err(store::db)?;
            Ok(json!({"bookmarked": false}))
        }
        WriteArgs::Alert { enabled } => {
            // Same upsert as POST /api/v1/alerts/preferences.
            sqlx::query(
                "INSERT INTO alert_subscriptions (id, user_id, program_id, enabled, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, now(), now())
                 ON CONFLICT (user_id, program_id) DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
            )
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(program)
            .bind(enabled)
            .execute(&mut *connection)
            .await
            .map_err(store::db)?;
            Ok(json!({"enabled": enabled}))
        }
        WriteArgs::Checklist { task_id, done } => {
            let plan = application_preparation::load_on(connection, user, program).await?;
            let mut completed = plan.completed_tasks.clone();
            completed.retain(|t| t != task_id);
            if *done {
                completed.push(task_id.clone());
            }
            let saved = application_preparation::save_on(
                connection,
                user,
                program,
                SavePreparation::new(plan.revision.clone(), completed, None),
            )
            .await?;
            Ok(
                json!({"task_id": task_id, "done": done, "completed": saved.completed_tasks.len(),
                "total": saved.tasks.len()}),
            )
        }
        WriteArgs::Status { status } => {
            let (state, ..) = crate::routes::state::record_status_on(
                connection,
                user,
                program,
                status.as_str(),
                None,
            )
            .await?;
            Ok(json!({"status": state}))
        }
        WriteArgs::Profile { field, value } => {
            let row = sqlx::query_as::<_, UserProfile>(
                "SELECT * FROM user_profiles WHERE user_id=$1 FOR UPDATE",
            )
            .bind(user)
            .fetch_one(&mut *connection)
            .await
            .map_err(store::db)?;
            let input = patched_profile(&row, *field, value)?;
            // Only the approved column, plus the district a region change
            // invalidates (shown on the card); the rest of the row is
            // untouched even if `normalize_profile` would rewrite it
            // (security review L2). Columns are constants.
            let set = |column: &str| {
                format!(
                    "UPDATE user_profiles SET {column}=$2, profile_version=profile_version+1, updated_at=now()
                     WHERE user_id=$1 RETURNING profile_version"
                )
            };
            let query = match field {
                ProfileField::RegionCode => sqlx::query_scalar::<_, i32>(
                    "UPDATE user_profiles SET region_code=$2, city_code=$3, profile_version=profile_version+1,
                       updated_at=now() WHERE user_id=$1 RETURNING profile_version",
                )
                .bind(user)
                .bind(input.region_code.clone())
                .bind(input.city_code.clone())
                .fetch_one(&mut *connection)
                .await,
                ProfileField::EnrollmentStatus => {
                    sqlx::query_scalar::<_, i32>(&set("enrollment_status"))
                        .bind(user)
                        .bind(input.enrollment_status.clone())
                        .fetch_one(&mut *connection)
                        .await
                }
                ProfileField::EmploymentStatus => {
                    sqlx::query_scalar::<_, i32>(&set("employment_status"))
                        .bind(user)
                        .bind(input.employment_status.clone())
                        .fetch_one(&mut *connection)
                        .await
                }
                ProfileField::SchoolYear => sqlx::query_scalar::<_, i32>(&set("school_year"))
                    .bind(user)
                    .bind(input.school_year)
                    .fetch_one(&mut *connection)
                    .await,
                ProfileField::IncomeBracket => {
                    sqlx::query_scalar::<_, i32>(&set("income_bracket"))
                        .bind(user)
                        .bind(input.income_bracket)
                        .fetch_one(&mut *connection)
                        .await
                }
                ProfileField::KosafSupportBracket => {
                    sqlx::query_scalar::<_, i32>(&set("kosaf_support_bracket"))
                        .bind(user)
                        .bind(input.kosaf_support_bracket)
                        .fetch_one(&mut *connection)
                        .await
                }
                ProfileField::HouseholdSize => {
                    sqlx::query_scalar::<_, i32>(&set("household_size"))
                        .bind(user)
                        .bind(input.household_size)
                        .fetch_one(&mut *connection)
                        .await
                }
            };
            let version = query.map_err(store::db)?;
            Ok(json!({"field": field, "value": value, "profile_version": version}))
        }
        WriteArgs::Calendar {
            calendar_date,
            date,
            days_before,
        } => {
            // Nothing on the server changes: the device writes the event
            // after its own calendar permission prompt. No user data in it.
            let proposal =
                super::calendar::resolve(connection, program, *calendar_date, *days_before)
                    .await
                    .map_err(|e| error(StatusCode::CONFLICT, &e.code, &e.message))?;
            Ok(json!({
                "device_write": "calendar",
                "event": {
                    "title": proposal.event_title,
                    "date": date,
                    "all_day": true,
                    "time_zone": "Asia/Seoul",
                    "notes": super::calendar::EVENT_NOTE,
                },
                "source": {"quote": proposal.quote, "label": proposal.source_label},
            }))
        }
        WriteArgs::Signature { .. } => {
            // Nothing on the server changes and no signature reaches it.
            let templates = form_templates(connection, user, program)
                .await
                .map_err(|e| error(StatusCode::CONFLICT, &e.code, &e.message))?;
            Ok(json!({
                "route": "/form-fill",
                "step": "signature",
                "signature": "on_device",
                "forms": templates.iter().map(|t| json!({
                    "resource_id": t.id, "label": t.label, "file_format": t.file_format,
                })).collect::<Vec<_>>(),
            }))
        }
        WriteArgs::Handoff {
            official_destination: url,
        } => Ok(json!({"device_open": "official_application", "url": url, "submitted": false})),
        WriteArgs::Form { .. } => {
            // Nothing on the server changes: the device builds the draft.
            let templates = form_templates(connection, user, program)
                .await
                .map_err(|e| error(StatusCode::CONFLICT, &e.code, &e.message))?;
            Ok(json!({
                "route": "/form-fill",
                "draft": "on_device",
                "forms": templates.iter().map(|t| json!({
                    "resource_id": t.id, "label": t.label, "file_format": t.file_format,
                })).collect::<Vec<_>>(),
            }))
        }
    }
}

fn done_message(preview: &ConfirmationPreview, args: &WriteArgs) -> String {
    let what = match args {
        WriteArgs::Bookmark { bookmarked: true } => "관심 혜택에 저장했어요.".to_owned(),
        WriteArgs::Bookmark { bookmarked: false } => "관심 혜택에서 뺐어요.".to_owned(),
        WriteArgs::Alert { enabled: true } => {
            "마감 알림을 켰어요. 실제 푸시는 알림 설정과 기기 권한에 따라 달라요.".to_owned()
        }
        WriteArgs::Alert { enabled: false } => "마감 알림을 껐어요.".to_owned(),
        WriteArgs::Checklist { done: true, .. } => "준비 항목을 완료로 표시했어요.".to_owned(),
        WriteArgs::Checklist { done: false, .. } => {
            "준비 항목의 완료 표시를 해제했어요.".to_owned()
        }
        WriteArgs::Status { status } => format!(
            "신청 상태를 ‘{}’(으)로 기록했어요. 기관 접수 확인은 아니에요.",
            status.label()
        ),
        WriteArgs::Profile { field, value } => format!(
            "‘{}’을(를) ‘{}’(으)로 바꿨어요. 추천과 조건 확인은 바뀐 프로필로 다시 계산돼요.",
            field.label(),
            field.value_label(Some(value))
        ),
        WriteArgs::Calendar {
            calendar_date,
            date,
            days_before,
        } => format!(
            "‘{}’ 일정({})을 이 기기 캘린더에 넣을게요. 캘린더 권한을 허용해야 추가돼요.",
            calendar_label(*calendar_date, *days_before),
            date.format("%Y. %-m. %-d.")
        ),
        WriteArgs::Signature { .. } => "작성 도우미의 서명 단계를 열어요. 서명은 이 기기에서만 하고 서버로 보내지 않아요. 제출은 공식 신청처에서 직접 해 주세요.".to_owned(),
        WriteArgs::Handoff { .. } => "공식 신청 페이지를 열어요. 로그인과 제출은 공식 사이트에서 직접 해 주세요. 마지미는 아무것도 제출하지 않았어요.".to_owned(),
        // The approved card's own 승인하면 text: one statement of the fill.
        WriteArgs::Form { .. } => format!(
            "서식 작성 도우미를 열어요. {}. 주민등록번호·계좌번호는 비워 둬요.",
            preview.after
        ),
    };
    format!("‘{}’: {what}", preview.title)
}

#[derive(Debug, Serialize)]
pub struct DecisionView {
    pub confirmation: ConfirmationView,
    pub run_status: String,
    /// The same approval or rejection was already recorded.
    pub replayed: bool,
    pub message: String,
}

fn conflict(code: &str, message: &str) -> Error {
    error(StatusCode::CONFLICT, code, message)
}

async fn lock_run(connection: &mut PgConnection, run: Uuid) -> Result<RunStatus, Error> {
    let status: String =
        sqlx::query_scalar("SELECT status FROM assistant_runs WHERE id=$1 FOR UPDATE")
            .bind(run)
            .fetch_one(&mut *connection)
            .await
            .map_err(store::db)?;
    RunStatus::parse(&status).ok_or_else(|| crate::errors::internal_error("unknown run status"))
}

async fn pending_left(connection: &mut PgConnection, run: Uuid) -> Result<bool, Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_confirmations WHERE run_id=$1 AND status='pending')",
    )
    .bind(run)
    .fetch_one(connection)
    .await
    .map_err(store::db)
}

/// After a decision: back to awaiting while others are pending, else done.
async fn settle_run(connection: &mut PgConnection, run: Uuid) -> Result<RunStatus, Error> {
    let next = if pending_left(connection, run).await? {
        RunStatus::AwaitingConfirmation
    } else {
        RunStatus::Completed
    };
    let current = lock_run(connection, run).await?;
    if current != next && current.can_transition(next) {
        store::transition(connection, run, next).await?;
        return Ok(next);
    }
    Ok(current)
}

async fn mark(
    connection: &mut PgConnection,
    row: &ConfirmationRow,
    status: &str,
    result: Option<&Value>,
    idempotency_key: Option<&str>,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE assistant_confirmations SET status=$2, decided_at=now(),
           executed_at = CASE WHEN $2='executed' THEN now() ELSE executed_at END,
           result=$3, idempotency_key=COALESCE($4, idempotency_key) WHERE id=$1",
    )
    .bind(row.id)
    .bind(status)
    .bind(result)
    .bind(idempotency_key)
    .execute(&mut *connection)
    .await
    .map_err(store::db)?;
    sqlx::query("UPDATE assistant_tool_calls SET status=$2 WHERE id=$1")
        .bind(row.tool_call_id)
        .bind(status)
        .execute(&mut *connection)
        .await
        .map_err(store::db)?;
    let outcome = match status {
        "executed" | "rejected" | "stale" | "failed" | "expired" => status,
        _ => return Ok(()),
    };
    store::audit(connection, row.user_id, row.id, &row.tool_name, outcome).await
}

async fn conversation_of(connection: &mut PgConnection, run: Uuid) -> Result<Uuid, Error> {
    sqlx::query_scalar("SELECT conversation_id FROM assistant_runs WHERE id=$1")
        .bind(run)
        .fetch_one(connection)
        .await
        .map_err(store::db)
}

fn verify_hash(row: &ConfirmationRow) -> bool {
    let Some(tool) = ToolName::parse(&row.tool_name) else {
        return false;
    };
    let expected = confirmation_hash(&ConfirmationBinding {
        user_id: row.user_id,
        run_id: row.run_id,
        tool_call_id: row.tool_call_id,
        tool,
        program_id: row.program_id,
        arguments: &row.arguments,
        precondition: &row.precondition,
        expires_at: row.expires_at,
    });
    expected
        .as_bytes()
        .ct_eq(row.payload_hash.as_bytes())
        .into()
}

pub async fn approve(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
    presented_hash: &str,
    idempotency_key: Option<&str>,
    tools: super::ToolFlags,
) -> Result<DecisionView, Error> {
    let mut tx = pool.begin().await.map_err(store::db)?;
    store::expire_due(&mut tx, user).await?;
    let row = store::lock_confirmation(&mut tx, user, id).await?;
    let run_status = lock_run(&mut tx, row.run_id).await?;
    match row.status.as_str() {
        "executed" => {
            let view = row.view();
            tx.commit().await.map_err(store::db)?;
            return Ok(DecisionView {
                message: "이미 승인해서 실행한 변경이에요.".into(),
                confirmation: view,
                run_status: run_status.as_str().into(),
                replayed: true,
            });
        }
        "pending" if row.expires_at <= chrono::Utc::now() => {
            // Expired while waiting for the lock: never execute late.
            mark(&mut tx, &row, "expired", None, idempotency_key).await?;
            settle_run(&mut tx, row.run_id).await?;
            tx.commit().await.map_err(store::db)?;
            return Err(conflict(
                "confirmation_expired",
                "확인 시간이 지났어요. 다시 요청해 주세요.",
            ));
        }
        "pending" => {}
        "expired" => {
            tx.commit().await.map_err(store::db)?;
            return Err(conflict(
                "confirmation_expired",
                "확인 시간이 지났어요. 다시 요청해 주세요.",
            ));
        }
        "rejected" => return Err(conflict("confirmation_rejected", "이미 취소한 변경이에요.")),
        "stale" => {
            return Err(conflict(
                "stale_confirmation",
                "그 사이 상태가 바뀌었어요. 다시 요청해 주세요.",
            ))
        }
        _ => {
            return Err(conflict(
                "confirmation_closed",
                "실행할 수 없는 변경이에요.",
            ))
        }
    }
    // A tool switched off is a kill switch for its pending cards too.
    if ToolName::parse(&row.tool_name).is_some_and(|tool| !tools.allows(tool)) {
        mark(&mut tx, &row, "expired", None, idempotency_key).await?;
        settle_run(&mut tx, row.run_id).await?;
        tx.commit().await.map_err(store::db)?;
        return Err(conflict(
            "confirmation_expired",
            "이 기능은 지금 쓸 수 없어요. 다시 요청해 주세요.",
        ));
    }
    let presented_ok: bool = presented_hash
        .as_bytes()
        .ct_eq(row.payload_hash.as_bytes())
        .into();
    if !presented_ok {
        return Err(conflict(
            "payload_mismatch",
            "확인한 내용과 실행할 내용이 달라요. 화면을 새로고침해 주세요.",
        ));
    }
    if !verify_hash(&row) {
        mark(&mut tx, &row, "failed", None, idempotency_key).await?;
        settle_run(&mut tx, row.run_id).await?;
        tx.commit().await.map_err(store::db)?;
        tracing::error!(confirmation = %row.id, "stored confirmation failed its own hash");
        return Err(conflict("payload_mismatch", "실행할 수 없는 변경이에요."));
    }
    if run_status != RunStatus::AwaitingConfirmation
        || !store::transition(&mut tx, row.run_id, RunStatus::Executing).await?
    {
        return Err(conflict(
            "run_not_awaiting",
            "이 요청은 더 이상 승인을 기다리지 않아요.",
        ));
    }
    let args: WriteArgs = serde_json::from_value(row.arguments.clone())
        .map_err(|_| crate::errors::internal_error("stored write arguments"))?;
    let conversation = conversation_of(&mut tx, row.run_id).await?;
    // Re-validate: the state now must be the state the user saw.
    let current = inspect(&mut tx, user, row.program_id, &args).await;
    let fresh = matches!(&current, Ok(i) if i.precondition == row.precondition && !i.no_op);
    if !fresh {
        mark(&mut tx, &row, "stale", None, idempotency_key).await?;
        let status = settle_run(&mut tx, row.run_id).await?;
        tx.commit().await.map_err(store::db)?;
        return Err((
            StatusCode::CONFLICT,
            axum::Json(
                json!({"error":"그 사이 공고나 상태가 바뀌어 실행하지 않았어요. 다시 요청해 주세요.",
                "code":"stale_confirmation","run_status":status.as_str()}),
            ),
        ));
    }
    let preview = current.map(|i| i.preview).ok();
    let mut savepoint = sqlx::Connection::begin(&mut *tx).await.map_err(store::db)?;
    let applied = apply(&mut savepoint, user, row.program_id, &args).await;
    let (status, result, message) = match applied {
        Ok(result) => {
            savepoint.commit().await.map_err(store::db)?;
            let message = preview
                .as_ref()
                .map(|p| done_message(p, &args))
                .unwrap_or_else(|| "변경했어요.".into());
            ("executed", Some(result), message)
        }
        Err((status, _)) => {
            savepoint.rollback().await.map_err(store::db)?;
            tracing::warn!(status = status.as_u16(), "confirmed agent write failed");
            (
                "failed",
                None,
                "변경하지 못했어요. 잠시 후 다시 시도해 주세요.".to_owned(),
            )
        }
    };
    mark(&mut tx, &row, status, result.as_ref(), idempotency_key).await?;
    store::add_message(
        &mut tx,
        conversation,
        row.run_id,
        "assistant",
        &message,
        &json!([]),
    )
    .await?;
    let run_status = settle_run(&mut tx, row.run_id).await?;
    let mut updated = row.clone();
    updated.status = status.into();
    updated.result = result;
    tx.commit().await.map_err(store::db)?;
    if status == "failed" {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "write_failed",
            &message,
        ));
    }
    Ok(DecisionView {
        confirmation: updated.view(),
        run_status: run_status.as_str().into(),
        replayed: false,
        message,
    })
}

pub async fn reject(pool: &PgPool, user: Uuid, id: Uuid) -> Result<DecisionView, Error> {
    let mut tx = pool.begin().await.map_err(store::db)?;
    store::expire_due(&mut tx, user).await?;
    let row = store::lock_confirmation(&mut tx, user, id).await?;
    let run_status = lock_run(&mut tx, row.run_id).await?;
    match row.status.as_str() {
        "rejected" => {
            let view = row.view();
            tx.commit().await.map_err(store::db)?;
            return Ok(DecisionView {
                confirmation: view,
                run_status: run_status.as_str().into(),
                replayed: true,
                message: "이미 취소한 변경이에요.".into(),
            });
        }
        "pending" => {}
        _ => {
            tx.commit().await.map_err(store::db)?;
            return Err(conflict(
                "confirmation_closed",
                "이미 처리됐거나 만료된 변경이에요.",
            ));
        }
    }
    mark(&mut tx, &row, "rejected", None, None).await?;
    let conversation = conversation_of(&mut tx, row.run_id).await?;
    let title = row
        .preview
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let message = format!("‘{title}’: 변경하지 않았어요.");
    store::add_message(
        &mut tx,
        conversation,
        row.run_id,
        "assistant",
        &message,
        &json!([]),
    )
    .await?;
    let run_status = settle_run(&mut tx, row.run_id).await?;
    let mut updated = row.clone();
    updated.status = "rejected".into();
    tx.commit().await.map_err(store::db)?;
    Ok(DecisionView {
        confirmation: updated.view(),
        run_status: run_status.as_str().into(),
        replayed: false,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use application_preparation::Task;

    fn task(id: &str, title: &str) -> Task {
        Task {
            id: id.into(),
            title: title.into(),
            detail: String::new(),
            kind: "document",
        }
    }

    #[test]
    fn tasks_resolve_by_id_or_unique_title_fragment() {
        let tasks = vec![
            task("document:재학증명서", "재학증명서"),
            task("document:주민등록등본", "주민등록 등본"),
            task("information", "신청 정보 준비"),
        ];
        assert_eq!(match_task(&tasks, "information").unwrap().id, "information");
        assert_eq!(
            match_task(&tasks, "재학증명서").unwrap().id,
            "document:재학증명서"
        );
        assert_eq!(
            match_task(&tasks, "주민등록등본").unwrap().id,
            "document:주민등록등본"
        );
        assert_eq!(
            match_task(&tasks, "통장").unwrap_err().code,
            "task_not_found"
        );
        let twins = vec![task("a", "성적증명서(국문)"), task("b", "성적증명서(영문)")];
        assert_eq!(
            match_task(&twins, "성적증명서").unwrap_err().code,
            "ambiguous_task"
        );
        assert_eq!(match_task(&tasks, "서").unwrap_err().code, "task_not_found");
    }

    #[test]
    fn settled_statuses_do_not_move_backwards() {
        use ApplicationStatus::*;
        assert!(status_transition_allowed(None, Applied));
        assert!(status_transition_allowed(Some("planning"), Applied));
        assert!(status_transition_allowed(Some("applied"), Received));
        assert!(!status_transition_allowed(Some("applied"), Planning));
        assert!(!status_transition_allowed(Some("received"), Abandoned));
        assert!(status_transition_allowed(Some("abandoned"), Applying));
        assert!(!status_transition_allowed(Some("abandoned"), Received));
    }

    #[test]
    fn write_args_round_trip_untagged() {
        for args in [
            WriteArgs::Bookmark { bookmarked: true },
            WriteArgs::Alert { enabled: false },
            WriteArgs::Checklist {
                task_id: "information".into(),
                done: true,
            },
            WriteArgs::Status {
                status: ApplicationStatus::Applied,
            },
            WriteArgs::Profile {
                field: ProfileField::IncomeBracket,
                value: "5".into(),
            },
            WriteArgs::Form { prefill: true },
            WriteArgs::Calendar {
                calendar_date: CalendarDate::Interview,
                date: chrono::NaiveDate::from_ymd_opt(2026, 12, 3).unwrap(),
                days_before: 0,
            },
            WriteArgs::Calendar {
                calendar_date: CalendarDate::Deadline,
                date: chrono::NaiveDate::from_ymd_opt(2026, 12, 2).unwrap(),
                days_before: 1,
            },
        ] {
            let value = serde_json::to_value(&args).unwrap();
            assert_eq!(serde_json::from_value::<WriteArgs>(value).unwrap(), args);
        }
    }
}
