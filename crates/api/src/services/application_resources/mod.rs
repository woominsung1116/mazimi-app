//! Program-specific application evidence. Routes never decide publication.
mod repository;

use axum::{http::StatusCode, Json};
use chrono::{DateTime, Duration, Utc};
use mazimi_core::{
    application_resources::{
        fingerprint, is_generic_portal, public_url, publication_host, Resource,
    },
    models::Program,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(&'static str),
    #[error("not found")]
    NotFound,
    #[error("review evidence changed")]
    Conflict,
    /// A template can only be approved against bytes the worker fetched.
    #[error("template probe required")]
    ProbeRequired,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
impl Error {
    pub fn response(self) -> (StatusCode, Json<Value>) {
        let (status, code, message) = match self {
            Self::Invalid(message) => (StatusCode::BAD_REQUEST, "invalid_resource", message),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "resource_not_found",
                "공고 또는 자료를 찾을 수 없습니다.",
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "resource_changed",
                "공고·자료가 변경됐거나 검증이 만료됐어요. 새로 확인해 주세요.",
            ),
            Self::ProbeRequired => (
                StatusCode::PRECONDITION_FAILED,
                "template_probe_required",
                "수집기가 최근 24시간 안에 이 양식 파일을 오류 없이 내려받아 형식·크기·해시를 확인한 뒤에만 승인할 수 있습니다. 출처 도메인 수집 승인을 확인하고 다음 자동 확인 뒤 다시 시도하세요.",
            ),
            Self::Database(error) => return crate::errors::internal_error(error),
        };
        (status, Json(json!({"error":message,"code":code})))
    }
}

#[derive(Debug, Serialize)]
pub struct PublicResource {
    pub id: Uuid,
    pub kind: String,
    pub url: String,
    pub source_url: String,
    pub label: String,
    pub access: String,
    pub file_format: Option<String>,
    pub byte_size: Option<i64>,
    pub content_hash: Option<String>,
    pub verified_at: chrono::DateTime<Utc>,
    pub valid_until: chrono::DateTime<Utc>,
}

#[derive(Debug, Default, Serialize)]
pub struct Resources {
    pub notice: Option<PublicResource>,
    pub application: Option<PublicResource>,
    pub templates: Vec<PublicResource>,
    pub status: &'static str,
    /// The worker's link selection (navigation only, never identity).
    #[serde(skip)]
    pub link: super::application_destination::LinkState,
}

impl Resources {
    /// Identity of what the user prepares against: which reviewed entries
    /// exist, where they lead and how they are accessed, plus a template's
    /// bytes. Freshness (`status`, `valid_until`) and display text are not
    /// identity: expiry or re-approval of the same entry must not reset a
    /// checklist (code review H1). Call it on `identity_at`, not `select_at`.
    /// An HTML page hash is worker bookkeeping that moves with page chrome.
    pub fn revision_material(&self) -> Value {
        let stable = |r: &PublicResource| {
            let hash = (r.kind == "template")
                .then_some(r.content_hash.as_ref())
                .flatten();
            json!({"id":r.id,"url":r.url,"access":r.access,"hash":hash})
        };
        let mut templates: Vec<Value> = self.templates.iter().map(stable).collect();
        templates.sort_by_key(|t| t["id"].as_str().map(str::to_owned));
        json!({"notice":self.notice.as_ref().map(stable),"application":self.application.as_ref().map(stable),
            "templates":templates})
    }
}

pub async fn load(connection: &mut PgConnection, p: &Program) -> Result<Resources, sqlx::Error> {
    let rows = repository::list_on(connection, p.id).await?;
    let rows = vouched(connection, p, rows).await?;
    let mut resources = select_at(&rows, p, Utc::now());
    resources.link = super::application_destination::load(connection, p.id).await?;
    Ok(resources)
}

/// What users see now, and the identity the checklist revision is built on.
pub async fn load_with_identity(
    connection: &mut PgConnection,
    p: &Program,
) -> Result<(Resources, Resources), sqlx::Error> {
    let rows = repository::list_on(connection, p.id).await?;
    let rows = vouched(connection, p, rows).await?;
    let now = Utc::now();
    let mut live = select_at(&rows, p, now);
    // Only what users see follows the resolver: a re-selected link must
    // not reset saved checklist progress, so the identity keeps `Pending`.
    live.link = super::application_destination::load(connection, p.id).await?;
    Ok((live, identity_at(&rows, p, now)))
}

/// Pure publication rule at `now`. `rows` come from one program.
pub fn select_at(rows: &[Resource], p: &Program, now: DateTime<Utc>) -> Resources {
    select(rows, p, now, false)
}

/// Same selection as `select_at` but an expired verification still counts:
/// only rejection, demotion, supersession or a new round change identity.
pub fn identity_at(rows: &[Resource], p: &Program, now: DateTime<Utc>) -> Resources {
    select(rows, p, now, true)
}

fn select(rows: &[Resource], p: &Program, now: DateTime<Utc>, identity: bool) -> Resources {
    let fp = fingerprint(p);
    let mut result = Resources {
        status: "unverified",
        ..Resources::default()
    };
    // No direct application or templates for an inactive/expired/future intake.
    let active = p.is_currently_available(now);
    let shown = |r: &&Resource| {
        if identity {
            r.published(&fp, now)
        } else {
            r.usable(&fp, now)
        }
    };
    for row in rows.iter().filter(shown) {
        let item = PublicResource {
            id: row.id,
            kind: row.kind.clone(),
            url: row.url.clone(),
            source_url: row.source_url.clone(),
            label: row.label.clone(),
            access: row.access.clone(),
            file_format: row.file_format.clone(),
            byte_size: row.byte_size,
            // A file's hash identifies the reviewed bytes; an HTML page hash
            // is worker bookkeeping and would only make clients reject a
            // still-valid link after a cosmetic recheck.
            content_hash: (row.kind == "template")
                .then(|| row.content_hash.clone())
                .flatten(),
            verified_at: row.verified_at.unwrap(),
            // `usable` rows always have it; an identity row is never
            // published to clients, so expiry there is not identity.
            valid_until: row.valid_until.unwrap_or(now),
        };
        match row.kind.as_str() {
            "notice" if result.notice.is_none() => result.notice = Some(item),
            "application" if active && result.application.is_none() => {
                result.application = Some(item)
            }
            "template" if active => result.templates.push(item),
            _ => {}
        }
    }
    if result.application.is_some() {
        result.status = "verified";
    } else if active
        // Outside the intake window nothing is offered, so nothing is being
        // rechecked either (code review L7).
        && rows
        .iter()
        // Only a destination users were once shown can need a "recheck";
        // an unreviewed candidate failing a probe is not news to users.
        // `verified_at` is kept as history after demotion or expiry.
        .any(|r| r.kind == "application" && r.status != "rejected" && r.verified_at.is_some())
    {
        result.status = "recheck_required";
    }
    result
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Queue {
    pub kind: String,
    pub url: String,
    pub source_url: String,
    pub label: String,
}
impl Queue {
    fn validate(&self) -> Result<(), Error> {
        if !matches!(self.kind.as_str(), "notice" | "application" | "template")
            || !public_url(&self.url)
            || !public_url(&self.source_url)
            || self.label.trim().is_empty()
            || self.label.chars().count() > 200
        {
            return Err(Error::Invalid(
                "유효한 HTTPS 주소·자료 종류·이름을 입력해 주세요.",
            ));
        }
        if self.kind == "application" && is_generic_portal(&self.url) {
            return Err(Error::Invalid(
                "홈페이지는 실제 신청 화면으로 등록할 수 없습니다.",
            ));
        }
        Ok(())
    }
}

pub async fn queue(pool: &PgPool, program: Uuid, input: Queue) -> Result<Resource, Error> {
    input.validate()?;
    let mut tx = pool.begin().await?;
    let p = repository::program(&mut tx, program, true)
        .await?
        .ok_or(Error::NotFound)?;
    if let Some(row) = repository::find(&mut tx, program, &input.kind, input.url.trim()).await? {
        return Ok(row);
    }
    // Rejected rows are history and do not count against the cap.
    if repository::live_count(&mut tx, program).await?
        >= mazimi_core::application_resources::MAX_RESOURCES
    {
        return Err(Error::Invalid("공고당 검토 대상 자료는 최대 40개입니다."));
    }
    // The worker inserts candidates without the program lock; a concurrent
    // insert of the same URL returns the existing row instead of a 500.
    let row = match repository::insert(&mut tx, &p, &input).await? {
        Some(row) => row,
        None => repository::find(&mut tx, program, &input.kind, input.url.trim())
            .await?
            .ok_or(Error::NotFound)?,
    };
    tx.commit().await?;
    Ok(row)
}

pub async fn list(pool: &PgPool, program: Uuid) -> Result<Value, Error> {
    let mut connection = pool.acquire().await?;
    let p = repository::program(&mut connection, program, false)
        .await?
        .ok_or(Error::NotFound)?;
    let rows = repository::list_on(&mut connection, program).await?;
    Ok(json!({"program_title":p.title,"program_fingerprint":fingerprint(&p),"items":rows}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub decision: String,
    pub expected_revision: i64,
    pub program_fingerprint: String,
    pub access: Option<String>,
    pub same_program_confirmed: bool,
    pub current_intake_confirmed: bool,
    pub destination_confirmed: bool,
    pub note: String,
    pub validity_hours: Option<i64>,
}

pub async fn review(
    pool: &PgPool,
    actor: Uuid,
    id: Uuid,
    input: Review,
) -> Result<Resource, Error> {
    if !matches!(input.decision.as_str(), "verified" | "rejected")
        || !(20..=2000).contains(&input.note.trim().chars().count())
    {
        return Err(Error::Invalid(
            "검토 결정과 20~2000자의 확인 근거를 입력해 주세요.",
        ));
    }
    let hours = input.validity_hours.unwrap_or(24);
    if !(1..=168).contains(&hours) {
        return Err(Error::Invalid("검증 유효기간은 1~168시간입니다."));
    }
    if input
        .access
        .as_deref()
        .is_some_and(|value| !matches!(value, "unknown" | "direct" | "login_required"))
    {
        return Err(Error::Invalid("유효한 접근 방식을 선택해 주세요."));
    }
    let mut tx = pool.begin().await?;
    // Consistent lock order with candidate inserts and program edits.
    let program_id = repository::resource_program(&mut tx, id)
        .await?
        .ok_or(Error::NotFound)?;
    let p = repository::program(&mut tx, program_id, true)
        .await?
        .ok_or(Error::NotFound)?;
    let row = repository::get_locked(&mut tx, id)
        .await?
        .ok_or(Error::NotFound)?;
    if row.revision != input.expected_revision || fingerprint(&p) != input.program_fingerprint {
        return Err(Error::Conflict);
    }
    let access = input.access.as_deref().unwrap_or("unknown");
    if input.decision == "verified" {
        if !input.same_program_confirmed
            || !input.current_intake_confirmed
            || !input.destination_confirmed
            || !matches!(access, "direct" | "login_required")
        {
            return Err(Error::Invalid(
                "사업·모집 회차·실제 신청 화면 또는 양식을 모두 확인해 주세요.",
            ));
        }
        if let Some(refusal) = verification_refusal(&mut tx, &p, &row, Utc::now()).await? {
            return Err(refusal);
        }
    }
    let updated = repository::save_review(&mut tx, &row, &p, actor, &input, hours).await?;
    tx.commit().await?;
    Ok(updated)
}

/// Why verifying `row` for `p` would be refused on the row itself (its URLs, its host, a
/// template's collector probe), or `None`. The one predicate a verification and the automatic
/// importer's dry run share (api::form_auto_import::preconditions).
pub async fn verification_refusal(
    connection: &mut PgConnection,
    p: &Program,
    row: &Resource,
    now: DateTime<Utc>,
) -> Result<Option<Error>, sqlx::Error> {
    if !public_url(&row.url)
        || !public_url(&row.source_url)
        || (row.kind == "application" && is_generic_portal(&row.url))
    {
        return Ok(Some(Error::Invalid(
            "사업·모집 회차·실제 신청 화면 또는 양식을 모두 확인해 주세요.",
        )));
    }
    if let Some(message) = host_refusal(connection, p, &row.url).await? {
        return Ok(Some(Error::Invalid(message)));
    }
    // Application destinations are often JS or login-only; a failed bot
    // probe is not evidence against them, so their check_error does not
    // block a human review. A template is approved against fetched bytes.
    if row.kind == "template" && !template_probe_fresh(row, p, now) {
        return Ok(Some(Error::ProbeRequired));
    }
    Ok(None)
}

/// Why `url` may not be published for `p`, or `None` when its host is
/// vouched for. Security M1 / code M4: the host must belong to the
/// program's ingested official site or to an approved source domain. An
/// admin-written official_url, a shared-hosting platform and a look-alike
/// (IDN/punycode) host never vouch for themselves; the last two need an
/// approved entry for that exact host.
async fn host_refusal(
    connection: &mut PgConnection,
    p: &Program,
    url: &str,
) -> Result<Option<&'static str>, sqlx::Error> {
    let Some(rule) = publication_host(url, p.official_url.as_deref()) else {
        return Ok(Some("주소의 호스트를 확인할 수 없습니다."));
    };
    let official = rule.official && !repository::official_url_admin_set(connection, p.id).await?;
    if official || repository::approved_domain(connection, &rule.host, rule.exact).await? {
        return Ok(None);
    }
    Ok(Some(if rule.idn {
        "국제화 도메인(xn--) 주소는 승인된 출처 도메인으로 등록된 경우에만 승인할 수 있습니다."
    } else {
        "수집된 공고 공식 주소와 같은 기관 도메인이거나 승인된 출처 도메인의 주소만 승인할 수 있습니다. 관리자가 입력한 공식 주소와 블로그·폼 같은 공유 호스팅은 승인 도메인 등록이 필요합니다."
    }))
}

/// Review M6: the host rule of an approval holds at read time too. A
/// verified row whose host is no longer vouched for (its source-domain
/// approval revoked, lapsed or deleted) is treated as expired: hidden, still
/// the same identity, shown again once the host is approved again.
async fn vouched(
    connection: &mut PgConnection,
    p: &Program,
    mut rows: Vec<Resource>,
) -> Result<Vec<Resource>, sqlx::Error> {
    use mazimi_core::application_resources::{automatic_apply_host, AUTOMATED_REVIEWER};
    let mut official: Option<Option<String>> = None;
    for row in rows.iter_mut().filter(|r| r.status == "verified") {
        // A link the worker verified by itself is held to the worker's host
        // rule (the official page's institution, a government domain, or a
        // listed public institution or bank); an admin-typed official URL
        // never vouches for it.
        if row.reviewed_by == Some(AUTOMATED_REVIEWER) {
            if official.is_none() {
                let admin_set = repository::official_url_admin_set(connection, p.id).await?;
                official = Some(p.official_url.clone().filter(|_| !admin_set));
            }
            if automatic_apply_host(official.as_ref().and_then(|o| o.as_deref()), &row.url) {
                continue;
            }
        }
        if host_refusal(connection, p, &row.url).await?.is_some() {
            row.valid_until = None;
        }
    }
    Ok(rows)
}

pub fn template_probe_fresh(row: &Resource, p: &Program, now: DateTime<Utc>) -> bool {
    row.program_fingerprint == fingerprint(p)
        && row.content_hash.is_some()
        && row.source_hash.is_some()
        && row.file_format.is_some()
        && row.byte_size.is_some()
        && row.check_error.is_none()
        && row
            .checked_at
            .is_some_and(|d| d >= now - Duration::hours(24))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(now: DateTime<Utc>) -> Program {
        Program {
            id: Uuid::new_v4(),
            program_type: "policy".into(),
            source_type: "manual".into(),
            source_id: Some("R1".into()),
            title: "2026년 청년 지원".into(),
            summary: None,
            provider_name: Some("부산광역시".into()),
            official_url: Some("https://www.busan.go.kr/notice/1".into()),
            program_status: "active".into(),
            application_start_at: None,
            application_end_at: Some(now + Duration::days(30)),
            benefit_amount_monthly: None,
            benefit_amount_semester: None,
            benefit_amount_once: None,
            region_scope: None,
            school_scope: None,
            tags: None,
            raw_payload: None,
            normalized_payload: None,
            min_age: None,
            max_age: None,
            regions: None,
            deadline_at: None,
            benefit_valid_until: None,
            is_active: true,
            last_synced_at: None,
            created_at: now,
            updated_at: now,
            company_name: None,
            company_logo_url: None,
            benefit_category: None,
            provider_kind: None,
            application_steps: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            region_districts: None,
            eligibility_condition: None,
            holder_requirement: None,
            age_basis: None,
        }
    }
    fn row(p: &Program, kind: &str, now: DateTime<Utc>) -> Resource {
        Resource {
            id: Uuid::new_v4(),
            program_id: p.id,
            kind: kind.into(),
            url: format!("https://apply.busan.go.kr/{kind}/1"),
            source_url: "https://www.busan.go.kr/notice/1".into(),
            label: "신청하기".into(),
            status: "verified".into(),
            access: "direct".into(),
            program_fingerprint: fingerprint(p),
            discovery_depth: 0,
            content_hash: Some("page-hash".into()),
            source_hash: Some("source".into()),
            file_format: (kind == "template").then(|| "pdf".into()),
            byte_size: (kind == "template").then_some(10),
            checked_at: Some(now),
            verified_at: Some(now - Duration::hours(1)),
            valid_until: Some(now + Duration::hours(1)),
            reviewed_by: Some(Uuid::new_v4()),
            review_note: None,
            check_error: None,
            check_status: Some("ok".into()),
            consecutive_failures: 0,
            consecutive_gone: 0,
            last_failure_code: None,
            revision: 1,
        }
    }

    #[test]
    fn select_at_respects_verification_and_intake_boundaries() {
        let now: DateTime<Utc> = "2026-10-08T00:00:00Z".parse().unwrap();
        let p = program(now);
        let mut app = row(&p, "application", now);
        assert_eq!(select_at(&[app.clone()], &p, now).status, "verified");
        // valid_until is exclusive (strict), verified_at inclusive up to the
        // bounded clock-skew tolerance (database clock ahead of this host).
        app.valid_until = Some(now);
        assert_eq!(
            select_at(&[app.clone()], &p, now).status,
            "recheck_required"
        );
        app.valid_until = Some(now + Duration::hours(1));
        app.verified_at = Some(now);
        assert_eq!(select_at(&[app.clone()], &p, now).status, "verified");
        let skew = mazimi_core::application_resources::CLOCK_SKEW_TOLERANCE;
        app.verified_at = Some(now + skew);
        assert_eq!(select_at(&[app.clone()], &p, now).status, "verified");
        app.verified_at = Some(now + skew + Duration::seconds(1));
        assert!(select_at(&[app.clone()], &p, now).application.is_none());
        app.verified_at = Some(now - Duration::hours(1));
        // Intake end is exclusive and start inclusive: outside the window no
        // application or template is offered, the reviewed notice still is.
        for (start, end, open) in [
            (None, Some(now), false),
            (None, Some(now + Duration::seconds(1)), true),
            (Some(now), None, true),
            (Some(now + Duration::seconds(1)), None, false),
        ] {
            let mut q = p.clone();
            q.application_start_at = start;
            q.application_end_at = end;
            let rows = ["application", "notice", "template"].map(|kind| row(&q, kind, now));
            let picked = select_at(&rows, &q, now);
            assert_eq!(picked.application.is_some(), open, "{start:?} {end:?}");
            assert_eq!(picked.templates.len(), usize::from(open));
            assert!(picked.notice.is_some());
        }
        // A new intake round changes the fingerprint: the old review is void.
        let mut next_round = p.clone();
        next_round.application_end_at = Some(now + Duration::days(400));
        assert!(select_at(&[app], &next_round, now).application.is_none());
    }

    #[test]
    fn recheck_required_only_for_destinations_users_were_shown() {
        let now = Utc::now();
        let p = program(now);
        let mut never = row(&p, "application", now);
        never.status = "candidate".into();
        never.verified_at = None;
        never.valid_until = None;
        never.check_status = Some("blocked".into());
        assert_eq!(select_at(&[never.clone()], &p, now).status, "unverified");
        let mut demoted = row(&p, "application", now);
        demoted.status = "unavailable".into();
        demoted.valid_until = None;
        assert_eq!(select_at(&[demoted], &p, now).status, "recheck_required");
        let mut rejected = row(&p, "application", now);
        rejected.status = "rejected".into();
        assert_eq!(select_at(&[rejected], &p, now).status, "unverified");
        // Code review L7: a closed or upcoming intake shows no destination at
        // all, so it is not "being rechecked".
        let mut closed = p.clone();
        closed.application_end_at = Some(now - Duration::days(1));
        let mut expired = row(&closed, "application", now);
        expired.valid_until = Some(now - Duration::hours(1));
        assert_eq!(select_at(&[expired], &closed, now).status, "unverified");
    }

    #[test]
    fn page_hashes_do_not_enter_revision_or_public_payload_but_template_hashes_do() {
        let now = Utc::now();
        let p = program(now);
        let app = row(&p, "application", now);
        let template = row(&p, "template", now);
        let before = select_at(&[app.clone(), template.clone()], &p, now);
        assert!(before.application.as_ref().unwrap().content_hash.is_none());
        assert_eq!(
            before.templates[0].content_hash.as_deref(),
            Some("page-hash")
        );
        let mut probed = app.clone();
        probed.content_hash = Some("first-probe".into());
        probed.checked_at = Some(now + Duration::minutes(5));
        let after = select_at(&[probed, template.clone()], &p, now);
        assert_eq!(before.revision_material(), after.revision_material());
        let mut new_file = template;
        new_file.content_hash = Some("new-bytes".into());
        let replaced = select_at(&[app, new_file], &p, now);
        assert_ne!(before.revision_material(), replaced.revision_material());
    }

    #[test]
    fn identity_ignores_expiry_and_reapproval_but_not_demotion() {
        let now = Utc::now();
        let p = program(now);
        let app = row(&p, "application", now);
        let base = identity_at(std::slice::from_ref(&app), &p, now).revision_material();
        let mut expired = app.clone();
        expired.valid_until = Some(now - Duration::seconds(1));
        assert!(select_at(&[expired.clone()], &p, now).application.is_none());
        assert_eq!(identity_at(&[expired], &p, now).revision_material(), base);
        let mut reapproved = app.clone();
        reapproved.verified_at = Some(now);
        reapproved.revision = 5;
        reapproved.label = "다시 확인한 신청하기".into();
        assert_eq!(
            identity_at(&[reapproved], &p, now).revision_material(),
            base
        );
        let mut demoted = app.clone();
        demoted.status = "unavailable".into();
        assert_ne!(identity_at(&[demoted], &p, now).revision_material(), base);
        let mut login = app;
        login.access = "login_required".into();
        assert_ne!(identity_at(&[login], &p, now).revision_material(), base);
    }

    /// The programme's official site covers `host` (same registrable domain,
    /// except on shared hosting).
    fn within_official_domain(host: &str, p: &Program) -> bool {
        publication_host(&format!("https://{host}/"), p.official_url.as_deref())
            .is_some_and(|rule| rule.official)
    }

    #[test]
    fn official_domain_binding_uses_registrable_domain() {
        let p = program(Utc::now());
        assert!(within_official_domain("apply.busan.go.kr", &p));
        assert!(within_official_domain("www.busan.go.kr", &p));
        assert!(!within_official_domain("busan-go-kr.example.com", &p));
        assert!(!within_official_domain("seoul.go.kr", &p));
        let mut blog = p.clone();
        blog.official_url = Some("https://blog.naver.com/busan_youth/1".into());
        assert!(!within_official_domain("form.naver.com", &blog));
        let mut none = p.clone();
        none.official_url = None;
        assert!(!within_official_domain("www.busan.go.kr", &none));
    }

    /// Review M6: a resource approved because its host was an approved
    /// source domain is hidden once that approval is revoked or lapses, as
    /// an expired one is (users see "recheck"), and shown again once the
    /// host is approved again. One on the programme's own official site
    /// needs no approval entry.
    #[tokio::test]
    async fn revoking_a_source_domain_hides_what_was_published_on_it() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let tag = Uuid::new_v4().simple().to_string();
        let host = format!("forms-{tag}.example.net");
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_end_at)
                     VALUES($1,'policy','manual','revocation fixture','active',true,'https://official.example.org/notice',now()+interval '30 days')")
            .bind(id).execute(&pool).await.unwrap();
        let p: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let domain: Uuid = sqlx::query_scalar("INSERT INTO benefit_source_domains(organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at)
              VALUES('revocation fixture',$1,'brand_official','verified','fixture',now(),'approved','written_permission','fixture',now(),'explicit_permission','fixture',now(),now()+interval '30 days') RETURNING id")
            .bind(&host).fetch_one(&pool).await.unwrap();
        for (kind, url) in [
            ("application", format!("https://{host}/apply")),
            (
                "notice",
                "https://official.example.org/notice/2026".to_string(),
            ),
        ] {
            sqlx::query("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,status,access,checked_at,verified_at,valid_until,reviewed_by,review_note,check_status)
                VALUES($1,$2,$3,'https://official.example.org/notice','신청하기',$4,'verified','direct',now(),now()-interval '1 hour',now()+interval '1 day',$5,
                  '동일 사업과 현재 모집 회차의 신청 화면을 확인한 테스트 근거입니다.','ok')")
                .bind(id).bind(kind).bind(&url).bind(fingerprint(&p)).bind(Uuid::new_v4())
                .execute(&pool).await.unwrap();
        }
        let shown = |pool: sqlx::PgPool, p: Program| async move {
            let mut connection = pool.acquire().await.unwrap();
            load(&mut connection, &p).await.unwrap()
        };
        let before = shown(pool.clone(), p.clone()).await;
        assert_eq!(before.status, "verified");
        assert!(before.application.is_some() && before.notice.is_some());

        for revoke in [
            "UPDATE benefit_source_domains SET compliance_status='rejected' WHERE id=$1",
            "UPDATE benefit_source_domains SET approval_expires_at=now()-interval '1 minute' WHERE id=$1",
            "UPDATE benefit_source_domains SET is_active=false WHERE id=$1",
            "DELETE FROM benefit_source_domains WHERE id=$1",
        ] {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query(revoke).bind(domain).execute(&mut *tx).await.unwrap();
            let rows = repository::list_on(&mut tx, p.id).await.unwrap();
            let after = select_at(&vouched(&mut tx, &p, rows).await.unwrap(), &p, Utc::now());
            tx.rollback().await.unwrap();
            assert!(after.application.is_none(), "{revoke}");
            assert_eq!(after.status, "recheck_required", "{revoke}");
            assert!(after.notice.is_some(), "official site needs no entry: {revoke}");
        }
        // Rolled back: the approval is in force again and so is the entry.
        assert!(shown(pool.clone(), p.clone()).await.application.is_some());
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
            .bind(domain)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[test]
    fn template_probe_must_be_recent_clean_and_for_this_round() {
        let now = Utc::now();
        let p = program(now);
        let fresh = row(&p, "template", now);
        assert!(template_probe_fresh(&fresh, &p, now));
        let mut stale = fresh.clone();
        stale.checked_at = Some(now - Duration::hours(25));
        assert!(!template_probe_fresh(&stale, &p, now));
        let mut failed = fresh.clone();
        failed.check_error = Some("approval_required".into());
        assert!(!template_probe_fresh(&failed, &p, now));
        let mut unprobed = fresh;
        unprobed.content_hash = None;
        assert!(!template_probe_fresh(&unprobed, &p, now));
    }
}
