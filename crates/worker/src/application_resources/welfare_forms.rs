//! Form files of 복지로 programmes from the official open API, never from the
//! bokjiro.go.kr pages.
//!
//! 한국사회보장정보원 publishes each service's attachments in its detail API
//! (`basfrmList`, detail code 040 = 서식/자료), licence "이용허락범위 제한
//! 없음":
//! - 중앙부처: `NationalWelfaredetailedV001?callTp=D&servId=` (data.go.kr
//!   15090532): `servSeCode`, `servSeDetailNm` (file name), `servSeDetailLink`.
//! - 지자체: `LcgvWelfaredetailed?servId=` (15108347): `wlfareInfoDtlCd`,
//!   `wlfareInfoReldNm`, `wlfareInfoReldCn`.
//!
//! Each listed file becomes a template candidate of its programme. The file
//! is then fetched like any other candidate (approved scope, robots, pacing,
//! magic bytes), and the forms pipeline decides from its pages whether it is
//! a form; guides and ordinances in the same list are dropped there. The API
//! key travels only in the request query and never reaches a row or a log.

use anyhow::{Context, Result};
use mazimi_core::{
    application_resources::public_url, models::Program, source_policy::registrable_domain,
};
use reqwest::{Client, Url};
use serde::Deserialize;
use sqlx::PgPool;
use std::time::Duration;

use super::{discovery::Candidate, repository};
use crate::sources::http;

/// Detail code of 서식/자료 attachments in `basfrmList`.
const FORM_LIST_CODE: &str = "040";
/// The only host a listed file may be on.
const FILE_DOMAIN: &str = "bokjiro.go.kr";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    National,
    Local,
}

impl Api {
    pub fn source_type(self) -> &'static str {
        match self {
            Api::National => "national_welfare",
            Api::Local => "local_welfare",
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Api::National => "https://apis.data.go.kr/B554287/NationalWelfareInformationsV001/NationalWelfaredetailedV001",
            Api::Local => "https://apis.data.go.kr/B554287/LocalGovernmentWelfareInformations/LcgvWelfaredetailed",
        }
    }
    /// The service key. Each name is read as a literal so the service env
    /// check (`scripts/check-service-env.py`) sees it.
    fn key(self) -> Option<String> {
        match self {
            Api::National => std::env::var("NATIONAL_WELFARE_API_KEY"),
            Api::Local => std::env::var("LOCAL_WELFARE_API_KEY"),
        }
        .ok()
    }
    /// Detail calls per hourly run. The national API's development quota is
    /// 100 calls a day, the local one's 1,000.
    fn per_run(self) -> i64 {
        let (value, default, max) = match self {
            Api::National => (
                std::env::var("APPLICATION_RESOURCES_WELFARE_NATIONAL_PER_RUN"),
                3,
                4,
            ),
            Api::Local => (
                std::env::var("APPLICATION_RESOURCES_WELFARE_LOCAL_PER_RUN"),
                30,
                40,
            ),
        };
        value
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(default)
            .clamp(0, max)
    }
}

#[derive(Debug, Deserialize, Default)]
struct Detail {
    #[serde(rename = "basfrmList", default)]
    files: Vec<Listed>,
}

/// One `basfrmList` entry in either schema.
#[derive(Debug, Deserialize, Default)]
struct Listed {
    #[serde(rename = "servSeCode", alias = "wlfareInfoDtlCd", default)]
    code: Option<String>,
    #[serde(rename = "servSeDetailNm", alias = "wlfareInfoReldNm", default)]
    name: Option<String>,
    #[serde(rename = "servSeDetailLink", alias = "wlfareInfoReldCn", default)]
    link: Option<String>,
}

/// The listed form files of one detail response: code 040, a public https
/// URL on bokjiro.go.kr, and a file name. Anything else (an error payload,
/// another code, a foreign link) yields nothing.
pub fn listed_files(xml: &str) -> Vec<Candidate> {
    let Ok(detail) = quick_xml::de::from_str::<Detail>(xml) else {
        return vec![];
    };
    let mut out: Vec<Candidate> = vec![];
    for f in detail.files {
        let (Some(code), Some(name), Some(link)) = (f.code, f.name, f.link) else {
            continue;
        };
        let (name, link) = (name.trim(), link.trim());
        let on_bokjiro = Url::parse(link).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host_str().and_then(registrable_domain).as_deref() == Some(FILE_DOMAIN)
        });
        if code.trim() != FORM_LIST_CODE || name.is_empty() || !on_bokjiro || !public_url(link) {
            continue;
        }
        if !out.iter().any(|c| c.url == link) {
            out.push(Candidate {
                kind: "template",
                url: link.to_string(),
                label: name.chars().take(200).collect(),
            });
        }
    }
    out
}

/// data.go.kr's error envelope (`<OpenAPI_ServiceResponse><cmmMsgHeader>`,
/// quota or key errors, sent with HTTP 200), or a detail whose `resultCode`
/// is not success. A detail without `resultCode` (the national API) is fine.
pub fn api_error(xml: &str) -> bool {
    if xml.contains("<cmmMsgHeader>") || xml.contains("<OpenAPI_ServiceResponse>") {
        return true;
    }
    let code = xml
        .split_once("<resultCode>")
        .and_then(|(_, rest)| rest.split_once("</resultCode>"))
        .map(|(code, _)| code.trim());
    code.is_some_and(|code| !matches!(code, "0" | "00" | "000"))
}

/// A 복지로 service id as the source stores it (`raw_payload.servId`).
pub fn service_id(p: &Program) -> Option<String> {
    let id = p.raw_payload.as_ref()?.get("servId")?.as_str()?.trim();
    (id.len() <= 20
        && id.starts_with("WLF")
        && id[3..].chars().all(|c| c.is_ascii_digit())
        && id.len() > 3)
        .then(|| id.to_string())
}

/// Asks the detail API for the programmes of one source that were not asked
/// in the last seven days, at most the per-run budget, and queues their
/// listed files. Returns the number of new candidates. A missing key skips
/// the source; an API failure stops it for this run (the next run retries).
pub async fn seed(pool: &PgPool, api: Api) -> Result<usize> {
    let Some(key) = api.key() else {
        return Ok(0);
    };
    let per_run = api.per_run();
    let programs = repository::welfare_programs_due(pool, api.source_type(), per_run).await?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(http::public_api_user_agent())
        .build()
        .context("welfare forms client")?;
    let mut added = 0;
    for (i, p) in programs.iter().enumerate() {
        let Some(id) = service_id(p) else {
            repository::welfare_forms_scanned(pool, p.id).await?;
            continue;
        };
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(1100)).await; // at most one call a second
        }
        let mut query = vec![("serviceKey", key.as_str()), ("servId", id.as_str())];
        if api == Api::National {
            query.push(("callTp", "D"));
        }
        let response = match http::send(client.get(api.endpoint()).query(&query), api.source_type())
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(source = api.source_type(), error = %error, "welfare detail API failed; next run retries");
                break;
            }
        };
        let text = http::text(response, api.source_type()).await?;
        if api_error(&text) {
            // Quota, key or service error with HTTP 200: nothing was learned about
            // this programme, so it is not stamped; the next run retries.
            tracing::warn!(
                source = api.source_type(),
                "welfare detail API answered an error payload; next run retries"
            );
            break;
        }
        added += repository::queue_files(pool, p, &listed_files(&text)).await?;
        repository::welfare_forms_scanned(pool, p.id).await?;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIONAL: &str =
        include_str!("../../tests/fixtures/welfare_forms/national_WLF00000024.xml");
    const LOCAL: &str = include_str!("../../tests/fixtures/welfare_forms/local_WLF00004882.xml");

    #[test]
    fn both_detail_schemas_list_their_form_files() {
        let national = listed_files(NATIONAL);
        assert!(!national.is_empty());
        assert!(national
            .iter()
            .all(|c| c.kind == "template" && c.url.starts_with("https://bokjiro.go.kr/ssis-tbu/")));
        assert!(national
            .iter()
            .any(|c| c.label == "사회보장급여 신청(변경)서.pdf"));
        // XML entities are decoded: the second query parameter survives.
        assert!(national.iter().all(|c| c.url.contains("&atcflSn=")));
        let local = listed_files(LOCAL);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].label, "무주군 장사 등에 관한 조례.hwp");
        assert!(local[0]
            .url
            .starts_with("https://www.bokjiro.go.kr/ssis-tbu/CmmFileUtil/siteQnaInfoDownload.do?"));
    }

    #[test]
    fn other_codes_foreign_hosts_and_error_payloads_list_nothing() {
        let entry = |code: &str, link: &str| {
            format!("<wantedDtl><basfrmList><servSeCode>{code}</servSeCode><servSeDetailLink>{link}</servSeDetailLink><servSeDetailNm>신청서.hwp</servSeDetailNm></basfrmList></wantedDtl>")
        };
        assert_eq!(
            listed_files(&entry("040", "https://www.bokjiro.go.kr/a.do?x=1")).len(),
            1
        );
        assert!(listed_files(&entry("010", "https://www.bokjiro.go.kr/a.do?x=1")).is_empty());
        assert!(listed_files(&entry("040", "https://bokjiro.go.kr.evil.example/a.do")).is_empty());
        assert!(listed_files(&entry("040", "http://www.bokjiro.go.kr/a.do")).is_empty());
        assert!(listed_files(&entry("040", "https://127.0.0.1/a.do")).is_empty());
        assert!(listed_files("<OpenAPI_ServiceResponse><cmmMsgHeader><errMsg>SERVICE ERROR</errMsg></cmmMsgHeader></OpenAPI_ServiceResponse>").is_empty());
        assert!(listed_files("not xml").is_empty());
    }

    #[test]
    fn an_error_payload_is_not_a_listing() {
        assert!(api_error("<OpenAPI_ServiceResponse><cmmMsgHeader><errMsg>LIMITED_NUMBER_OF_SERVICE_REQUESTS_EXCEEDS_ERROR</errMsg><returnReasonCode>22</returnReasonCode></cmmMsgHeader></OpenAPI_ServiceResponse>"));
        assert!(api_error("<wantedDtl><resultCode>30</resultCode><resultMessage>SERVICE_KEY_IS_NOT_REGISTERED</resultMessage></wantedDtl>"));
        assert!(!api_error(NATIONAL));
        assert!(
            !api_error(LOCAL),
            "the local API's own resultCode 0 is success"
        );
    }

    #[test]
    fn only_a_well_formed_service_id_is_asked_about() {
        let program = |serv: serde_json::Value| -> Program {
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000002", "program_type": "welfare", "source_type": "national_welfare",
                "title": "아이돌봄서비스", "program_status": "active", "is_active": true,
                "created_at": "2026-10-31T00:00:00Z", "updated_at": "2026-10-31T00:00:00Z",
                "raw_payload": {"servId": serv},
            }))
            .unwrap()
        };
        assert_eq!(
            service_id(&program(serde_json::json!("WLF00000024"))).as_deref(),
            Some("WLF00000024")
        );
        for bad in [
            serde_json::json!("unknown"),
            serde_json::json!("WLF"),
            serde_json::json!("WLF1&x=2"),
            serde_json::json!(null),
        ] {
            assert_eq!(service_id(&program(bad.clone())), None, "{bad}");
        }
    }
}
