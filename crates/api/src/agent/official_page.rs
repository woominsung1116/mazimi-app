//! `check_official_page`: compare a programme's stored data with its
//! official page, fetched live by the isolated fetch service over MCP.
//!
//! The API keeps every decision that needs data or identity:
//! - the programme comes from a token the run resolved for the JWT user
//!   (another user's `saved` never reaches here);
//! - only URLs already stored for the programme are candidates, and only
//!   those inside an approved `benefit_source_domains` scope are sent
//!   (`live_page::approved_targets`); a user never supplies a URL;
//! - the shared per-domain request slot is reserved here (the collectors'
//!   own politeness budget), plus a per-user hourly budget and a 10-minute
//!   cache per programme URL;
//! - the comparison with our snapshot and the wording are server templates.
//!
//! The fetch service holds no database credentials and gets no user data:
//! a programme id (correlation) and the targets. Its quotes are page text:
//! bounded, sanitized again here, shown as quotes, never sent to a model.
use super::{
    tools::{tool_error, EvidenceIds, Outcome},
    EvidenceRef, ToolError,
};
use chrono::{DateTime, NaiveDate, Utc};
use mazimi_core::official_page::{
    attachment_key, sanitize_quote, CheckRequest, CheckResponse, IntakeStatus,
    Outcome as FetchOutcome, MAX_QUOTES, TOOL_NAME,
};
use mazimi_worker::sources::live_page;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Live checks per user per hour that reach the network (cache hits are free).
pub const CHECKS_PER_USER_PER_HOUR: usize = 10;
const CACHE_TTL: Duration = Duration::from_secs(600);
const CACHE_CAP: usize = 2_000;
/// The whole MCP call; inside the agent's 15 s tool timeout with the slot wait.
const CALL_TIMEOUT: Duration = Duration::from_secs(11);
const SLOT_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveCheckError {
    Unreachable,
    Invalid,
}

/// The fetch service as the API sees it.
#[async_trait::async_trait]
pub trait LiveCheck: Send + Sync {
    async fn check(&self, request: &CheckRequest) -> Result<CheckResponse, LiveCheckError>;
}

/// MCP client over the Streamable HTTP transport (plain HTTP on the
/// internal network, shared bearer token). One short session per call.
pub struct McpLiveCheck {
    endpoint: String,
    token: String,
}

impl McpLiveCheck {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            token: token.into(),
        }
    }
}

#[async_trait::async_trait]
impl LiveCheck for McpLiveCheck {
    async fn check(&self, request: &CheckRequest) -> Result<CheckResponse, LiveCheckError> {
        use rmcp::{
            model::{CallToolRequestParams, ClientConfig},
            transport::{
                streamable_http_client::StreamableHttpClientTransportConfig,
                StreamableHttpClientTransport,
            },
            ServiceExt,
        };
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.endpoint.clone())
                .auth_header(self.token.clone()),
        );
        let client: rmcp::service::RunningService<rmcp::RoleClient, ClientConfig> =
            ClientConfig::default()
                .serve(transport)
                .await
                .map_err(|_| LiveCheckError::Unreachable)?;
        let arguments = match serde_json::to_value(request) {
            Ok(Value::Object(map)) => map,
            _ => return Err(LiveCheckError::Invalid),
        };
        let result = client
            .call_tool(CallToolRequestParams::new(TOOL_NAME).with_arguments(arguments))
            .await;
        let _ = client.cancel().await;
        let result = result.map_err(|_| LiveCheckError::Unreachable)?;
        if result.is_error == Some(true) {
            return Err(LiveCheckError::Invalid);
        }
        result.into_typed().map_err(|_| LiveCheckError::Invalid)
    }
}

/// Process-local state of the live check (like the agent's other limits).
pub struct OfficialPages {
    checker: Option<Arc<dyn LiveCheck>>,
    cache: Mutex<HashMap<String, (Instant, CheckResponse)>>,
    users: Mutex<HashMap<Uuid, Vec<Instant>>>,
}

impl OfficialPages {
    pub fn new(checker: Option<Arc<dyn LiveCheck>>) -> Self {
        Self {
            checker,
            cache: Mutex::new(HashMap::new()),
            users: Mutex::new(HashMap::new()),
        }
    }

    /// `OFFICIAL_PAGE_MCP_URL` (internal, e.g. `http://official-page:8090/mcp`)
    /// and `OFFICIAL_PAGE_MCP_TOKEN` (32+ chars). Missing or malformed: no
    /// checker, and the tool answers that it could not check. Whether the
    /// tool is offered at all is `ASSISTANT_AGENT_OFFICIAL_PAGE_ENABLED`
    /// (`runtime::ToolFlags`).
    pub fn from_env() -> Self {
        let endpoint = std::env::var("OFFICIAL_PAGE_MCP_URL").unwrap_or_default();
        let token = std::env::var("OFFICIAL_PAGE_MCP_TOKEN").unwrap_or_default();
        let checker = (internal_endpoint(&endpoint) && token.len() >= 32)
            .then(|| Arc::new(McpLiveCheck::new(endpoint, token)) as Arc<dyn LiveCheck>);
        Self::new(checker)
    }

    pub fn available(&self) -> bool {
        self.checker.is_some()
    }

    fn cached(&self, key: &str) -> Option<CheckResponse> {
        self.cache
            .lock()
            .ok()?
            .get(key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, response)| response.clone())
    }

    fn store(&self, key: String, response: CheckResponse) {
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= CACHE_CAP {
                cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
                if cache.len() >= CACHE_CAP {
                    cache.clear();
                }
            }
            cache.insert(key, (Instant::now(), response));
        }
    }

    fn refund(&self, user: Uuid) {
        if let Ok(mut users) = self.users.lock() {
            if let Some(times) = users.get_mut(&user) {
                times.pop();
            }
        }
    }

    fn admit(&self, user: Uuid) -> bool {
        let Ok(mut users) = self.users.lock() else {
            return false;
        };
        let now = Instant::now();
        let hour = Duration::from_secs(3600);
        if users.len() >= 20_000 {
            users.retain(|_, times| {
                times.retain(|at| now.duration_since(*at) < hour);
                !times.is_empty()
            });
        }
        let times = users.entry(user).or_default();
        times.retain(|at| now.duration_since(*at) < hour);
        if times.len() >= CHECKS_PER_USER_PER_HOUR {
            return false;
        }
        times.push(now);
        true
    }
}

/// The service must be a plain-HTTP internal name, never a public URL.
fn internal_endpoint(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|h| !h.contains('.') || h == "127.0.0.1")
}

/// KST calendar date of a timestamp.
/// Cache key for one reading: the KST date is part of it, so a page read
/// at 23:55 is not served at 00:01 when "today" has moved past a date.
fn cache_key(program: Uuid, url: &str, now: DateTime<Utc>) -> String {
    format!("{program}|{}|{url}", kst_date(now))
}

pub(crate) fn kst_date(at: DateTime<Utc>) -> NaiveDate {
    (at + chrono::Duration::hours(9)).date_naive()
}

fn korean_date(date: NaiveDate) -> String {
    use chrono::Datelike;
    format!("{}. {}. {}.", date.year(), date.month(), date.day())
}

struct Snapshot {
    title: String,
    urls: Vec<String>,
    official_url: Option<String>,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    attachments: Vec<String>,
    captured_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ProgramRow {
    title: String,
    official_url: Option<String>,
    apply: Option<String>,
    ref1: Option<String>,
    ref2: Option<String>,
    application_start_at: Option<DateTime<Utc>>,
    application_end_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

fn db_error(error: sqlx::Error) -> ToolError {
    tracing::warn!(error = %error, "official page snapshot query failed");
    tool_error(
        "tool_failed",
        "정보를 불러오지 못했어요. 잠시 후 다시 시도해 주세요.",
    )
}

async fn snapshot(pool: &PgPool, program: Uuid) -> Result<Snapshot, ToolError> {
    let row: ProgramRow = sqlx::query_as(
        "SELECT title, official_url, raw_payload->>'aplyUrlAddr' AS apply,
                raw_payload->>'refUrlAddr1' AS ref1, raw_payload->>'refUrlAddr2' AS ref2,
                application_start_at, application_end_at, deadline_at, updated_at
         FROM programs WHERE id=$1",
    )
    .bind(program)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| {
        tool_error(
            "program_unavailable",
            "지금 안내할 수 있는 혜택이 아니에요.",
        )
    })?;
    // Review M5: the same publication rule as the public programme page
    // (this round's review, unexpired, unchanged file, approved host).
    let resources = async {
        let p: Option<mazimi_core::models::Program> =
            sqlx::query_as("SELECT * FROM programs WHERE id=$1")
                .bind(program)
                .fetch_optional(pool)
                .await?;
        let Some(p) = p else {
            return Ok(crate::services::application_resources::Resources::default());
        };
        let mut connection = pool.acquire().await?;
        crate::services::application_resources::load(&mut connection, &p).await
    }
    .await
    .map_err(db_error)?;
    // The link shown is the resolver's checked one (`shown_url`); the fetch
    // targets below are the isolated check's own candidates.
    let official_url = crate::services::application_destination::shown_url(
        row.official_url.as_deref(),
        &resources.link,
    );
    let mut attachments: Vec<String> = resources.templates.into_iter().map(|t| t.label).collect();
    attachments.sort();
    attachments.truncate(20);
    let mut urls: Vec<String> = Vec::new();
    // The verified notice page first: it is this intake's own page; the
    // official URL is often a portal or list page (code review M1).
    for url in resources
        .notice
        .map(|n| n.url)
        .into_iter()
        .chain(row.official_url.clone())
        .chain([row.apply, row.ref1, row.ref2].into_iter().flatten())
    {
        let url = url.trim().to_owned();
        if mazimi_core::application_resources::public_url(&url) && !urls.contains(&url) {
            urls.push(url);
        }
    }
    Ok(Snapshot {
        title: row.title,
        official_url,
        urls,
        start: row.application_start_at.map(kst_date),
        end: row.application_end_at.or(row.deadline_at).map(kst_date),
        attachments,
        captured_at: row.updated_at,
    })
}

/// Why no live reading is shown. Each becomes the same plain sentence
/// ("지금 공식 페이지를 확인하지 못했어요") with the official link.
fn not_checked(snapshot: &Snapshot, program: Uuid, reason: &str) -> Outcome {
    Outcome {
        summary: json!({"live_status": "not_checked", "reason": reason}),
        data: json!({
            "program_id": program,
            "title": snapshot.title,
            "live_status": "not_checked",
            "reason": reason,
            "official_url": snapshot.official_url,
            "changes": [],
            "quotes": [],
        }),
        evidence: Vec::new(),
    }
}

/// What differs between our snapshot and the page.
fn changes(snapshot: &Snapshot, findings: &mazimi_core::official_page::Findings) -> Vec<Value> {
    let mut out = Vec::new();
    match findings.status {
        IntakeStatus::Closed => {
            out.push(json!({"field": "status", "ours": "신청 기간", "live": "마감"}))
        }
        IntakeStatus::Upcoming => {
            out.push(json!({"field": "status", "ours": "신청 기간", "live": "접수 전"}))
        }
        _ => {}
    }
    if let Some(live) = findings.period_end {
        if snapshot.end != Some(live) {
            out.push(json!({
                "field": "deadline",
                "ours": snapshot.end.map(korean_date),
                "live": korean_date(live),
            }));
        }
    }
    if !snapshot.attachments.is_empty() && !findings.attachments.is_empty() {
        let known = |name: &str, list: &[String]| {
            let key = attachment_key(name);
            list.iter().any(|other| {
                let other = attachment_key(other);
                other.contains(&key) || key.contains(&other)
            })
        };
        let added: Vec<&String> = findings
            .attachments
            .iter()
            .filter(|a| !known(a, &snapshot.attachments))
            .collect();
        let missing: Vec<&String> = snapshot
            .attachments
            .iter()
            .filter(|a| !known(a, &findings.attachments))
            .collect();
        if !added.is_empty() {
            out.push(json!({"field": "attachments_added", "live": added}));
        }
        if !missing.is_empty() {
            out.push(json!({"field": "attachments_missing", "ours": missing}));
        }
    }
    out
}

/// Run the tool for one resolved programme.
pub async fn check(
    pool: &PgPool,
    pages: &OfficialPages,
    user: Uuid,
    program: Uuid,
    ids: &mut EvidenceIds,
) -> Result<Outcome, ToolError> {
    let snapshot = snapshot(pool, program).await?;
    let targets = live_page::approved_targets(pool, &snapshot.urls)
        .await
        .map_err(db_error)?;
    let Some((scope, first)) = targets.first().cloned() else {
        return Ok(not_checked(&snapshot, program, "source_not_approved"));
    };
    let Some(checker) = pages.checker.clone() else {
        return Ok(not_checked(&snapshot, program, "service_unavailable"));
    };
    let key = cache_key(program, &first.url, Utc::now());
    let (response, cached) = match pages.cached(&key) {
        Some(response) => (response, true),
        None => {
            if !pages.admit(user) {
                return Ok(not_checked(&snapshot, program, "rate_limited"));
            }
            if !live_page::reserve_slot(pool, scope, first.min_interval_ms, SLOT_WAIT).await {
                // No request was made: the check is not charged.
                pages.refund(user);
                return Ok(not_checked(&snapshot, program, "cooldown"));
            }
            let request = CheckRequest {
                program_id: program.to_string(),
                targets: targets.into_iter().map(|(_, t)| t).collect(),
            };
            match tokio::time::timeout(CALL_TIMEOUT, checker.check(&request)).await {
                Ok(Ok(response)) => {
                    // The site asked to be left alone: the collectors and
                    // later checks wait too.
                    if let Some(until) = response.retry_after {
                        if let Err(error) = live_page::defer_domain(pool, scope, until).await {
                            tracing::warn!(error = %error, "domain pause not recorded");
                        }
                    }
                    if response.outcome == FetchOutcome::Fetched {
                        pages.store(key, response.clone());
                    }
                    (response, false)
                }
                Ok(Err(_)) | Err(_) => {
                    tracing::warn!("official page service did not answer");
                    return Ok(not_checked(&snapshot, program, "service_unavailable"));
                }
            }
        }
    };
    let (FetchOutcome::Fetched, Some(findings)) = (response.outcome, response.findings.as_ref())
    else {
        return Ok(not_checked(&snapshot, program, response.outcome.as_str()));
    };
    // The page URL is one of ours; still only a public HTTPS URL leaves here.
    let safe = |url: Option<&str>| crate::services::benefit_assistant::safe_official_url(url);
    let source_url = safe(response.url.as_deref())
        .filter(|url| {
            snapshot
                .urls
                .iter()
                .any(|ours| safe(Some(ours)).as_ref() == Some(url))
        })
        .or_else(|| snapshot.official_url.clone());
    let mut quotes: Vec<(&str, String)> = findings
        .status_quotes
        .iter()
        .map(|q| ("접수 상태", sanitize_quote(q)))
        .collect();
    if let Some(period) = &findings.period_quote {
        quotes.insert(0, ("접수 기간", sanitize_quote(period)));
    }
    quotes.truncate(MAX_QUOTES);
    let evidence: Vec<EvidenceRef> = quotes
        .iter()
        .map(|(label, text)| EvidenceRef {
            id: ids.id(),
            program_id: Some(program),
            kind: "live_official_page".into(),
            quote: Some(text.clone()),
            source_label: format!("공식 페이지 실시간 확인 · {label}"),
            source_url: source_url.clone(),
            captured_at: Some(response.fetched_at),
        })
        .collect();
    let changes = changes(&snapshot, findings);
    Ok(Outcome {
        summary: json!({"live_status": findings.status.as_str(), "changes": changes.len(), "cached": cached}),
        data: json!({
            "program_id": program,
            "title": snapshot.title,
            "live_status": findings.status.as_str(),
            "checked_at": response.fetched_at,
            "cached": cached,
            "source_url": source_url,
            "official_url": snapshot.official_url,
            "quotes": quotes.iter().map(|(label, text)| json!({"label": label, "text": text})).collect::<Vec<_>>(),
            "ours": {
                "start": snapshot.start,
                "deadline": snapshot.end,
                "attachments": snapshot.attachments,
                "captured_at": snapshot.captured_at,
            },
            "live": {
                "start": findings.period_start,
                "deadline": findings.period_end,
                "attachments": findings.attachments.iter().map(|a| sanitize_quote(a)).collect::<Vec<_>>(),
            },
            "changes": changes,
        }),
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazimi_core::official_page::Findings;

    fn day(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn snapshot(end: Option<NaiveDate>, attachments: &[&str]) -> Snapshot {
        Snapshot {
            title: "t".into(),
            urls: vec![],
            official_url: None,
            start: None,
            end,
            attachments: attachments.iter().map(|a| (*a).to_owned()).collect(),
            captured_at: Utc::now(),
        }
    }

    fn findings(status: IntakeStatus, end: Option<NaiveDate>, attachments: &[&str]) -> Findings {
        Findings {
            status,
            status_quotes: vec![],
            period_quote: None,
            period_start: None,
            period_end: end,
            attachments: attachments.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    /// Review M5: the assistant reads verified notices and forms through
    /// the same publication rule as the public programme page: a review for
    /// an earlier round, an expired verification and a changed file are not
    /// offered as this intake's page or attachment.
    #[tokio::test]
    async fn the_snapshot_uses_the_public_resource_rule() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_end_at)
                     VALUES($1,'policy','manual','assistant snapshot fixture','active',true,'https://official.example.org/notice',now()+interval '30 days')")
            .bind(id).execute(&pool).await.unwrap();
        let p: mazimi_core::models::Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let fp = mazimi_core::application_resources::fingerprint(&p);
        let rows = [
            (
                "notice",
                "https://official.example.org/notice/this-round",
                "공고",
                fp.as_str(),
                "1 day",
                "ok",
            ),
            (
                "notice",
                "https://official.example.org/notice/last-round",
                "지난 공고",
                "earlier-round",
                "1 day",
                "ok",
            ),
            (
                "template",
                "https://official.example.org/form.hwp",
                "신청서.hwp",
                fp.as_str(),
                "1 day",
                "ok",
            ),
            (
                "template",
                "https://official.example.org/old.hwp",
                "옛 신청서.hwp",
                fp.as_str(),
                "-1 hour",
                "ok",
            ),
            (
                "template",
                "https://official.example.org/new.hwp",
                "바뀐 양식.hwp",
                fp.as_str(),
                "1 day",
                "changed",
            ),
            (
                "template",
                "https://official.example.org/2025.hwp",
                "2025 양식.hwp",
                "earlier-round",
                "1 day",
                "ok",
            ),
        ];
        for (kind, url, label, fingerprint, valid, check) in rows {
            sqlx::query("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,status,access,content_hash,source_hash,file_format,byte_size,checked_at,verified_at,valid_until,reviewed_by,review_note,check_status)
                VALUES($1,$2,$3,'https://official.example.org/notice',$4,$5,'verified','direct','h','s',
                  CASE WHEN $2='template' THEN 'hwp' END,CASE WHEN $2='template' THEN 10 END,now(),now()-interval '2 hours',now()+$6::interval,$7,
                  '동일 사업과 현재 모집 회차의 공고와 양식을 확인한 테스트 근거입니다.',$8)")
                .bind(id).bind(kind).bind(url).bind(label).bind(fingerprint).bind(valid).bind(Uuid::new_v4()).bind(check)
                .execute(&pool).await.unwrap();
        }
        let read = super::snapshot(&pool, id).await.ok().unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            read.urls.first().map(String::as_str),
            Some("https://official.example.org/notice/this-round")
        );
        assert!(
            !read.urls.iter().any(|u| u.ends_with("last-round")),
            "{:?}",
            read.urls
        );
        assert_eq!(read.attachments, vec!["신청서.hwp".to_string()]);
    }

    #[test]
    fn changes_name_status_deadline_and_attachments() {
        let ours = snapshot(Some(day(11, 27)), &["신청서(서식).hwp"]);
        assert!(changes(
            &ours,
            &findings(
                IntakeStatus::Open,
                Some(day(11, 27)),
                &["신청서 (서식).hwp"]
            )
        )
        .is_empty());
        let moved = changes(&ours, &findings(IntakeStatus::Open, Some(day(11, 30)), &[]));
        assert_eq!(
            moved,
            [json!({"field":"deadline","ours":"2026. 11. 27.","live":"2026. 11. 30."})]
        );
        let closed = changes(
            &ours,
            &findings(IntakeStatus::Closed, None, &["동의서.hwp"]),
        );
        assert_eq!(closed[0]["field"], "status");
        assert_eq!(closed[1]["field"], "attachments_added");
        assert_eq!(closed[2]["field"], "attachments_missing");
        // No attachment list of ours: nothing to compare, nothing claimed.
        assert!(changes(
            &snapshot(None, &[]),
            &findings(IntakeStatus::Unknown, None, &["a.hwp"])
        )
        .is_empty());
    }

    #[test]
    fn the_service_endpoint_must_be_internal_plain_http() {
        assert!(internal_endpoint("http://official-page:8090/mcp"));
        assert!(internal_endpoint("http://127.0.0.1:8090/mcp"));
        for bad in [
            "https://official-page.example.com/mcp",
            "http://evil.example/mcp",
            "http://u:p@official-page:8090/mcp",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!internal_endpoint(bad), "{bad}");
        }
    }

    #[test]
    fn per_user_budget_and_cache() {
        let pages = OfficialPages::new(None);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..CHECKS_PER_USER_PER_HOUR {
            assert!(pages.admit(a));
        }
        assert!(!pages.admit(a));
        pages.refund(a);
        assert!(pages.admit(a), "a refunded check can be used again");
        assert!(pages.admit(b));
        let response = CheckResponse {
            outcome: FetchOutcome::Fetched,
            url: None,
            fetched_at: Utc::now(),
            findings: None,
            retry_after: None,
        };
        pages.store("k".into(), response.clone());
        assert_eq!(pages.cached("k"), Some(response));
        assert_eq!(pages.cached("other"), None);
    }

    /// Review LOW: a reading cached before midnight (KST) is not served
    /// the next day.
    #[test]
    fn the_cache_key_changes_at_midnight_kst() {
        use chrono::TimeZone;
        let p = Uuid::nil();
        let before = Utc.with_ymd_and_hms(2026, 10, 16, 14, 59, 0).unwrap(); // 23:59 KST
        let after = Utc.with_ymd_and_hms(2026, 10, 16, 15, 1, 0).unwrap(); // 00:01 KST
        assert_ne!(
            cache_key(p, "https://a.go.kr/x", before),
            cache_key(p, "https://a.go.kr/x", after)
        );
        let same = Utc.with_ymd_and_hms(2026, 10, 16, 3, 0, 0).unwrap();
        assert_eq!(
            cache_key(p, "https://a.go.kr/x", before),
            cache_key(p, "https://a.go.kr/x", same)
        );
    }
}
