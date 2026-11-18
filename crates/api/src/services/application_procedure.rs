//! The detail page's 신청 방법 (Figma 34065:11144) and 신청 진행 단계
//! (34065:10997), from the programme's own fields and the user's records.
//!
//! [`derive`] assembles the Figma 신청 방법 body: the organisation, the
//! "전화 문의 / 방문 신청 / 온라인 신청" lines, the application period, then
//! the notice's own procedure steps ([`mazimi_core::application_procedure`]).
//! A line or step exists only when a field states it.
//! [`progress`] reads what is recorded for one user; the stage states come
//! from the single function [`mazimi_core::application_procedure::stages`].
use super::application_destination;
use super::application_policy::ApplicationPolicy;
use super::application_workflow;
use chrono::{DateTime, FixedOffset, Utc};
use mazimi_core::application_procedure::{
    self as procedure, Progress, StageView, Step, Submission,
};
use mazimi_core::models::Program;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// One "label : value" line of the Figma body text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub label: &'static str,
    pub value: Option<String>,
    /// A checked public HTTPS link for the value.
    pub url: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplicationProcedure {
    /// The body's organisation line (Figma "서울주거포털").
    pub organization: Option<String>,
    pub lines: Vec<Line>,
    pub intro: Option<String>,
    pub steps: Vec<Step>,
    pub ordered: bool,
}

/// Longest upstream text copied into a line or the organisation (characters).
const RAW_TEXT_MAX_CHARS: usize = 300;

fn raw<'a>(program: &'a Program, path: &[&str]) -> Option<&'a str> {
    let mut value = program.raw_payload.as_ref()?;
    for key in path {
        value = value.get(key)?;
    }
    let text = value.as_str().map(str::trim).filter(|s| !s.is_empty())?;
    Some(match text.char_indices().nth(RAW_TEXT_MAX_CHARS) {
        Some((at, _)) => &text[..at],
        None => text,
    })
}

/// A site named in the upstream payload, as a destination (validated public
/// HTTPS). Its URL is never linked by itself: see [`derive`].
fn upstream_site(url: &str) -> Option<application_destination::Destination> {
    application_destination::resolve(Some(&url.trim().replace("&amp;", "&")))
}

fn registrable(url: &str) -> Option<String> {
    let parsed = mazimi_core::source_policy::canonicalize_public_https_url(url).ok()?;
    mazimi_core::source_policy::registrable_domain(&parsed.domain)
}

fn kst_date(at: DateTime<Utc>) -> String {
    at.with_timezone(&FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y.%m.%d")
        .to_string()
}

/// The application window: the stored dates, else 보조금24's own 신청기한 text.
fn period(program: &Program) -> Option<(String, &'static str)> {
    let end = program.application_end_at.or(program.deadline_at);
    match (program.application_start_at, end) {
        (Some(start), Some(end)) => Some((
            format!("{} ~ {}", kst_date(start), kst_date(end)),
            "application_period",
        )),
        (None, Some(end)) => Some((format!("~ {}", kst_date(end)), "application_period")),
        (Some(start), None) => Some((format!("{} ~", kst_date(start)), "application_period")),
        (None, None) => raw(program, &["신청기한"]).map(|t| (t.to_owned(), "신청기한")),
    }
}

/// 신청 방법 of one programme. `policy` is the detail page's own policy, so
/// the online line links only where the 신청하기 button does.
pub fn derive(program: &Program, policy: &ApplicationPolicy) -> ApplicationProcedure {
    let youth = program.source_type == "youth_center";
    let (method_source, screening_source) = if youth {
        ("plcyAplyMthdCn", "srngMthdCn")
    } else {
        ("application_method", "screening_method")
    };
    // An enumerated channel list ("기타 온라인신청||방문신청") is not a procedure.
    // 보조금24: the serviceDetail row's own 신청방법 is the procedure text; the
    // list's is channel codes. An enumerated channel list ("기타
    // 온라인신청||방문신청") is never a procedure.
    let method = raw(program, &["detail", "신청방법"])
        .filter(|_| program.source_type == "gov_benefits")
        .map(|m| (m, "serviceDetail.신청방법"))
        .or_else(|| {
            program
                .application_method
                .as_deref()
                .map(|m| (m, method_source))
        })
        .filter(|(m, _)| !application_workflow::enumerated_only(m));
    let screening = program
        .screening_method
        .as_deref()
        .map(|s| (s, screening_source));
    // 복지로 중앙: the detail API's stage list (worker sources/national_welfare.rs).
    let listed: Vec<(Option<&str>, &str)> = program
        .raw_payload
        .as_ref()
        .and_then(|raw| raw.pointer("/detail/applmetList"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    Some((
                        e.get("stage").and_then(Value::as_str),
                        e.get("text")?.as_str()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let steps = if listed.is_empty() {
        procedure::procedure_steps(method, screening)
    } else {
        procedure::listed_stages(&listed, "applmetList")
    };

    let mut lines = Vec::new();
    let contact = raw(program, &["전화문의"])
        .map(|t| (t.replace('/', " "), "전화문의"))
        .or_else(|| raw(program, &["rprsCtadr"]).map(|t| (t.to_owned(), "rprsCtadr")));
    if let Some((value, source)) = contact {
        lines.push(Line {
            label: "전화 문의",
            value: Some(value),
            url: None,
            source,
        });
    }
    // The online line links only where the 신청하기 button goes: the page's
    // own destination (vetted by the link resolver). A site the notice names
    // gives the line its name, and the link only when it is that same site.
    let destination = policy.destination.as_ref();
    let named = raw(program, &["detail", "온라인신청사이트URL"])
        .and_then(upstream_site)
        .map(|site| (site, "온라인신청사이트URL"))
        .or_else(|| {
            raw(program, &["aplyUrlAddr"])
                .and_then(upstream_site)
                .map(|site| (site, "aplyUrlAddr"))
        });
    let online_site: Option<(String, Option<String>, &'static str)> = match (named, destination) {
        (Some((site, source)), destination) => {
            let same = destination.filter(|d| {
                registrable(&d.url).is_some() && registrable(&d.url) == registrable(&site.url)
            });
            Some((
                application_destination::site_name(&site),
                same.map(|d| d.url.clone()),
                source,
            ))
        }
        (None, Some(d)) => Some((
            application_destination::site_name(d),
            Some(d.url.clone()),
            "destination",
        )),
        (None, None) => None,
    };
    let office = raw(program, &["접수기관"])
        .map(|t| (t, "접수기관"))
        .or_else(|| raw(program, &["detail", "접수기관명"]).map(|t| (t, "접수기관명")));
    for channel in &policy.workflow.channels {
        let Some(label) = channel_label(channel.id) else {
            continue;
        };
        let (value, url, source) = match channel.id {
            "online" => match &online_site {
                Some((name, url, source)) => (Some(name.clone()), url.clone(), *source),
                None => (None, None, "application_method"),
            },
            "visit" => match office {
                Some((name, source)) => (Some(name.to_owned()), None, source),
                None => (None, None, "application_method"),
            },
            _ => (None, None, "application_method"),
        };
        // The notice's own steps already say how; a bare channel name would repeat them.
        if steps.steps.is_empty() || value.is_some() {
            lines.push(Line {
                label,
                value,
                url,
                source,
            });
        }
    }
    if !policy.workflow.no_application_required {
        if let Some((value, source)) = period(program) {
            lines.push(Line {
                label: "신청 기간",
                value: Some(value),
                url: None,
                source,
            });
        }
    }
    let organization = raw(program, &["operInstCdNm"])
        .or_else(|| raw(program, &["bizChrDeptNm"]))
        .map(str::to_owned)
        .or_else(
            || match (raw(program, &["jurMnofNm"]), raw(program, &["jurOrgNm"])) {
                (Some(ministry), Some(office)) => Some(format!("{ministry} {office}")),
                (ministry, office) => ministry.or(office).map(str::to_owned),
            },
        );
    // A step that only names a channel ("온라인신청 / 기장군청 홈페이지") says
    // what a line, read off another field, already says; the line is the one
    // statement of a channel.
    let stated = |title: &str| {
        application_workflow::enumerated_only(title)
            && application_workflow::classify(Some(title))
                .channels
                .iter()
                .all(|c| {
                    channel_label(c.id).is_some_and(|l| lines.iter().any(|line| line.label == l))
                })
    };
    let steps_kept: Vec<Step> = steps
        .steps
        .into_iter()
        .filter(|s| !stated(&s.title))
        .collect();
    ApplicationProcedure {
        organization,
        intro: steps.intro,
        ordered: steps.ordered,
        steps: steps_kept,
        lines,
    }
}

/// The 신청 방법 line label of a channel; `None` for one without a line.
fn channel_label(id: &str) -> Option<&'static str> {
    Some(match id {
        "online" => "온라인 신청",
        "visit" => "방문 신청",
        "postal" => "우편 신청",
        "email" => "이메일 신청",
        "phone" => "전화 신청",
        "fax" => "팩스 신청",
        "mobile" => "앱 신청",
        _ => return None,
    })
}

/// The checklist's document tasks: listed documents, or the one "check the
/// documents" task when none are listed.
fn is_document_task(id: &str) -> bool {
    id.starts_with("document:") || id == "documents:confirm"
}

/// What is recorded for `user` and `program`; `None` when no such programme
/// exists. Reads only: the checklist (`application_preparations`; the
/// current plan's tasks, or the stored ticks of a closed programme), the
/// handoff and report (`application_journeys`), the user's own status
/// (`user_program_states`), and the latest connector submission
/// (`application_submissions`). A signed-in viewer of the detail page has
/// found the programme. Database errors are returned, never read as "no
/// progress".
pub async fn progress(
    connection: &mut sqlx::PgConnection,
    user: Uuid,
    program: Uuid,
) -> Result<Option<Progress>, super::application_preparation::Error> {
    let db = crate::errors::internal_error;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM programs WHERE id=$1)")
        .bind(program)
        .fetch_one(&mut *connection)
        .await
        .map_err(db)?;
    if !exists {
        return Ok(None);
    }
    let mut p = Progress {
        discovered: true,
        ..Default::default()
    };
    match super::application_preparation::load_on(connection, user, program).await {
        Ok(plan) => {
            let done = |id: &str| plan.completed_tasks.iter().any(|t| t == id);
            p.eligibility_confirmed = done("eligibility");
            let documents: Vec<&str> = plan
                .tasks
                .iter()
                .map(|t| t.id.as_str())
                .filter(|id| is_document_task(id))
                .collect();
            p.documents_total = documents.len();
            p.documents_done = documents.iter().filter(|id| done(id)).count();
            p.documents_not_required = documents.is_empty();
        }
        // Closed or not yet open: no current plan, but the user's ticks stand.
        Err((status, _)) if status == axum::http::StatusCode::NOT_FOUND => {
            let ticked: Option<Vec<String>> = sqlx::query_scalar(
                "SELECT completed_tasks FROM application_preparations WHERE user_id=$1 AND program_id=$2",
            )
            .bind(user)
            .bind(program)
            .fetch_optional(&mut *connection)
            .await
            .map_err(db)?;
            let ticked = ticked.unwrap_or_default();
            p.eligibility_confirmed = ticked.iter().any(|t| t == "eligibility");
            p.documents_done = ticked.iter().filter(|t| is_document_task(t)).count();
        }
        Err(error) => return Err(error),
    }
    let journey: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT outcome, receipt_reference FROM application_journeys WHERE user_id=$1 AND program_id=$2",
    )
    .bind(user)
    .bind(program)
    .fetch_optional(&mut *connection)
    .await
    .map_err(db)?;
    let state: Option<String> = sqlx::query_scalar(
        "SELECT state FROM user_program_states WHERE user_id=$1 AND program_id=$2",
    )
    .bind(user)
    .bind(program)
    .fetch_optional(&mut *connection)
    .await
    .map_err(db)?;
    let submission: Option<String> = sqlx::query_scalar(
        "SELECT state FROM application_submissions WHERE user_id=$1 AND program_id=$2 ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(user)
    .bind(program)
    .fetch_optional(&mut *connection)
    .await
    .map_err(db)?;
    p.handed_off = journey.is_some();
    p.reported_applied = journey
        .as_ref()
        .is_some_and(|(outcome, receipt)| outcome == "reported_applied" || receipt.is_some())
        || matches!(state.as_deref(), Some("applied" | "waiting" | "received"));
    p.preparing = state.as_deref() == Some("applying");
    p.submission = submission.as_deref().and_then(Submission::parse);
    Ok(Some(p))
}

/// The stepper of a guest: every stage ahead, 발견 highlighted.
pub fn guest_stages() -> Vec<StageView> {
    procedure::stages(&Progress::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::application_policy::with_resources;
    use serde_json::json;

    fn program(source_type: &str, fields: Value) -> Program {
        let now = Utc::now();
        let mut base = json!({
            "id": Uuid::nil(), "program_type": "policy", "source_type": source_type,
            "title": "t", "program_status": "active", "is_active": true,
            "created_at": now, "updated_at": now
        });
        base.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let mut p: Program = serde_json::from_value(base.clone()).unwrap();
        // raw_payload is skip_serializing but deserializes; set it explicitly.
        p.raw_payload = base.get("raw_payload").cloned();
        p
    }

    fn derived(p: &Program) -> ApplicationProcedure {
        derive(p, &with_resources(p, Default::default()))
    }

    fn line<'a>(d: &'a ApplicationProcedure, label: &str) -> Option<&'a Line> {
        d.lines.iter().find(|l| l.label == label)
    }

    /// 보조금24 기장군 청년 면접수당 지원 (svcId 340000000130), production row.
    fn gijang() -> Program {
        program(
            "gov_benefits",
            json!({
                "title": "기장군 청년 면접수당 지원",
                "official_url": "https://plus.gov.kr/portal/benefitV2/benefitTotalSrvcList/benefitSrvcDtl?svcSeq=2128527&bnefType=all&svcId=340000000130",
                "application_method": "기타 온라인신청",
                "application_start_at": "2026-02-01T15:00:00Z",
                "application_end_at": "2026-12-10T14:59:59Z",
                "raw_payload": {
                    "detail": {
                        "구비서류": "1. [행정정보 공동이용 사전동의서] 또는 [주민등록표 등초본, 건강보험자격득실확인서] 중 선택\r\n2. 사실증명(사업자등록사실여부)\r\n3. 채용공고\r\n4. 면접확인서 및 증빙자료\r\n5. 통장사본 등",
                        "온라인신청사이트URL": "https://www.gijang.go.kr/index.gijang?menuCd=DOM_000000104009011001&amp;cpath="
                    },
                    "신청기한": "2026.02.02~2026.12.10",
                    "신청방법": "기타 온라인신청",
                    "전화문의": "기장군청 일자리경제과/051-709-4395",
                    "선정기준": null,
                    "접수기관": null
                }
            }),
        )
    }

    #[test]
    fn gijang_shows_its_contact_site_and_period_and_no_invented_steps() {
        let d = derived(&gijang());
        assert_eq!(
            d.lines,
            [
                Line {
                    label: "전화 문의",
                    value: Some("기장군청 일자리경제과 051-709-4395".into()),
                    url: None,
                    source: "전화문의"
                },
                Line {
                    label: "온라인 신청",
                    value: Some("gijang.go.kr".into()),
                    url: None,
                    source: "온라인신청사이트URL"
                },
                Line {
                    label: "신청 기간",
                    value: Some("2026.02.02 ~ 2026.12.10".into()),
                    url: None,
                    source: "application_period"
                },
            ]
        );
        // "기타 온라인신청" is a channel code, not a procedure: no 심사, no 지급.
        assert!(d.steps.is_empty());
        assert_eq!(d.organization, None);
    }

    /// The online line links only the page's own destination: a site the
    /// notice names is a name, linked only when it is the destination's
    /// site. Upstream text is capped.
    /// Production 2026-11-05: 보조금24's serviceDetail 신청방법 ("온라인신청 /
    /// 기장군청 홈페이지") named the channel the 온라인 신청 line already gives
    /// from 온라인신청사이트URL; the section said it twice.
    #[test]
    fn a_step_naming_only_a_stated_channel_is_not_repeated() {
        let mut p = gijang();
        p.raw_payload.as_mut().unwrap()["detail"]["신청방법"] =
            json!("온라인신청: 기장군청 홈페이지");
        let d = derived(&p);
        assert!(line(&d, "온라인 신청").is_some());
        assert!(d.steps.is_empty(), "{:?}", d.steps);
        // A step that says more than a channel stays.
        p.raw_payload.as_mut().unwrap()["detail"]["신청방법"] =
            json!("1. 기장군청 홈페이지 접속\n2. 신청서 작성 후 제출");
        assert_eq!(derived(&p).steps.len(), 2);
    }

    #[test]
    fn the_online_link_is_the_vetted_destination_only() {
        // 기장군: the named site is not the destination (the 보조금24 page).
        let d = derived(&gijang());
        let online = line(&d, "온라인 신청").unwrap();
        assert_eq!(online.value.as_deref(), Some("gijang.go.kr"));
        assert_eq!(online.url, None);
        // The destination is the named site: linked, to the destination's URL.
        let p = program(
            "youth_center",
            json!({
                "application_method": "온라인 신청",
                "official_url": "https://apply.artloan.kr/form?y=2026",
                "raw_payload": {"aplyUrlAddr": "https://www.artloan.kr/notice", "operInstCdNm": "가".repeat(400)}
            }),
        );
        let d = derived(&p);
        let online = line(&d, "온라인 신청").unwrap();
        assert_eq!(online.value.as_deref(), Some("artloan.kr"));
        assert_eq!(
            online.url.as_deref(),
            Some("https://apply.artloan.kr/form?y=2026")
        );
        assert_eq!(
            d.organization.as_deref().map(|o| o.chars().count()),
            Some(300)
        );
    }

    /// A programme whose only fact is "온라인 신청" (온통청년 2026 창업컨설팅).
    #[test]
    fn only_online_gives_one_bare_line_and_nothing_else() {
        let d = derived(&program(
            "youth_center",
            json!({"application_method": "온라인 신청"}),
        ));
        assert_eq!(
            d.lines,
            [Line {
                label: "온라인 신청",
                value: None,
                url: None,
                source: "application_method"
            }]
        );
        assert!(d.steps.is_empty());
    }

    /// 보조금24 rows with a visit channel and a 접수기관 (관할 세무서 row).
    #[test]
    fn a_visit_line_names_the_office_the_notice_gives() {
        let d = derived(&program(
            "gov_benefits",
            json!({
                "application_method": "방문신청",
                "raw_payload": {"신청기한": "상시신청", "전화문의": "해당지역 지자체(세무서)/관할 세무서", "접수기관": "관할 세무서"}
            }),
        ));
        assert_eq!(
            line(&d, "방문 신청").unwrap().value.as_deref(),
            Some("관할 세무서")
        );
        assert_eq!(
            line(&d, "신청 기간").unwrap().value.as_deref(),
            Some("상시신청")
        );
        assert_eq!(line(&d, "신청 기간").unwrap().source, "신청기한");
    }

    /// 복지로 지자체 (local_welfare) 부산 난임시술비지원사업: "방문, 인터넷", department only.
    #[test]
    fn bokjiro_local_welfare_lists_its_channels_and_department() {
        let d = derived(&program(
            "local_welfare",
            json!({
                "application_method": "방문, 인터넷",
                "official_url": "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00000001",
                "raw_payload": {"aplyMtdNm": "방문, 인터넷", "bizChrDeptNm": "부산광역시 시민건강국 건강정책과"}
            }),
        ));
        assert_eq!(
            d.organization.as_deref(),
            Some("부산광역시 시민건강국 건강정책과")
        );
        let labels: Vec<_> = d.lines.iter().map(|l| l.label).collect();
        assert_eq!(labels, ["방문 신청", "온라인 신청"]);
        assert_eq!(
            line(&d, "온라인 신청").unwrap().value.as_deref(),
            Some("복지로")
        );
        assert!(d.steps.is_empty());
    }

    /// 온통청년 부산 전세피해 임차인 주거안정지원금 지원: numbered steps from the notice.
    #[test]
    fn youth_center_text_becomes_the_procedure_and_bare_channels_drop() {
        let d = derived(&program(
            "youth_center",
            json!({
                "application_method": "\r\n① 신청대상자 안내 문자\r\n② 주거안정지원금 신청\r\n③ 적합여부 및 서류 검토\r\n④ 지급결정 통보\r\n⑤ 계좌 입금\r\n",
                "screening_method": "- 지원금 지급대상자로 결정 시, 결정 통보 및 지급 안내 문자 발송",
                "raw_payload": {"aplyUrlAddr": "www.gov.kr "}
            }),
        ));
        assert_eq!(d.steps.len(), 6);
        assert!(d.ordered);
        assert_eq!(d.steps[0].source, "plcyAplyMthdCn");
        assert_eq!(d.steps[5].source, "srngMthdCn");
        assert!(d.lines.iter().all(|l| l.value.is_some()));
    }

    #[test]
    fn no_application_required_has_no_period_line() {
        let d = derived(&program(
            "gov_benefits",
            json!({
                "application_method": "신청불필요",
                "raw_payload": {"신청기한": "별도의 신청절차가 없음"}
            }),
        ));
        assert!(d.lines.is_empty());
    }

    /// 보조금24 serviceDetail 신청방법 (production, 2026-10-31): numbered steps.
    #[test]
    fn gov24_detail_method_text_is_the_procedure() {
        let d = derived(&program(
            "gov_benefits",
            json!({
                "application_method": "방문신청",
                "raw_payload": {"신청방법": "방문신청", "detail": {"신청방법": "1. 소속 장기요양기관으로 신청서류 제출\r\n2. 장기요양기관에서 신청서류 취합 및 지원요건 1차 확인 \r\n3. 구청 신청서류 검토 및 접수 \r\n4. 요건 확인 후 개인별 지급"}}
            }),
        ));
        assert!(d.ordered);
        assert_eq!(d.steps.len(), 4);
        assert_eq!(d.steps[3].title, "요건 확인 후 개인별 지급");
        assert_eq!(d.steps[0].source, "serviceDetail.신청방법");
        // The bare channel line would repeat the steps.
        assert!(line(&d, "방문 신청").is_none());
        // A code-only detail value stays a channel line, never a step.
        let d = derived(&program(
            "gov_benefits",
            json!({"application_method": "방문신청", "raw_payload": {"detail": {"신청방법": "방문 신청"}}}),
        ));
        assert!(d.steps.is_empty());
        assert!(line(&d, "방문 신청").is_some());
    }

    /// 복지로 중앙 공공분양: the stored detail's stage list is the procedure.
    #[test]
    fn national_welfare_detail_becomes_the_procedure() {
        let d = derived(&program(
            "national_welfare",
            json!({
                "raw_payload": {"rprsCtadr": "1600-1004", "jurMnofNm": "국토교통부", "jurOrgNm": "공공주택정책과",
                    "detail": {"applmetList": [
                        {"stage": "결정기관연락처목록", "text": "담당 시/군/구청 또는 한국토지주택공사에서 보장 결정"},
                        {"stage": "신청기관연락처목록", "text": "거주지 읍/면/동 주민센터, 한국토지주택공사에서 ‘서비스 신청’"}
                    ]}}
            }),
        ));
        assert!(d.ordered);
        assert_eq!(
            d.steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
            ["서비스 신청", "보장 결정"]
        );
        assert_eq!(d.steps[0].source, "applmetList");
        assert_eq!(d.organization.as_deref(), Some("국토교통부 공공주택정책과"));
        assert_eq!(
            line(&d, "전화 문의").unwrap().value.as_deref(),
            Some("1600-1004")
        );
    }

    /// Coverage over a production export (one JSON row per line, the
    /// columns `derive` reads). `PROCEDURE_EXPORT=… cargo test -p api --lib
    /// procedure_coverage -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn procedure_coverage() {
        let path = std::env::var("PROCEDURE_EXPORT").expect("PROCEDURE_EXPORT");
        let text = std::fs::read_to_string(path).unwrap();
        let (mut total, mut steps, mut ordered, mut lines) =
            (0usize, [0usize; 5], [0usize; 5], 0usize);
        let mut by_source: std::collections::BTreeMap<String, [usize; 3]> = Default::default();
        for row in text.lines().filter(|l| l.starts_with('{')) {
            let row: Value = serde_json::from_str(row).unwrap();
            let p = program(row["source_type"].as_str().unwrap(), row.clone());
            let d = derived(&p);
            total += 1;
            let n = d.steps.len().min(4);
            steps[n] += 1;
            if d.ordered {
                ordered[n] += 1;
            }
            if !d.lines.is_empty() || !d.steps.is_empty() {
                lines += 1;
            }
            let e = by_source.entry(p.source_type.clone()).or_default();
            e[0] += 1;
            if d.steps.len() >= 2 {
                e[1] += 1;
            }
            if !d.lines.is_empty() || !d.steps.is_empty() {
                e[2] += 1;
            }
        }
        println!("total {total}; 신청 방법 has any line or step: {lines}");
        for n in 0..5 {
            println!(
                "steps {}{}: {} (ordered {})",
                n,
                if n == 4 { "+" } else { "" },
                steps[n],
                ordered[n]
            );
        }
        for (source, [n, two, any]) in by_source {
            println!("{source}: {n} rows, 2+ steps {two}, any content {any}");
        }
    }

    /// `derive` output for the export rows, keyed by id (UI check fixtures).
    #[test]
    #[ignore]
    fn procedure_fixture() {
        let path = std::env::var("PROCEDURE_EXPORT").expect("PROCEDURE_EXPORT");
        let ids = std::env::var("PROCEDURE_IDS").unwrap_or_default();
        let mut out = serde_json::Map::new();
        for row in std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| l.starts_with('{'))
        {
            let row: Value = serde_json::from_str(row).unwrap();
            let id = row["id"].as_str().unwrap().to_owned();
            if ids.split(',').any(|i| i == id) {
                let p = program(row["source_type"].as_str().unwrap(), row.clone());
                out.insert(id, serde_json::to_value(derived(&p)).unwrap());
            }
        }
        println!("FIXTURE {}", Value::Object(out));
    }
}
