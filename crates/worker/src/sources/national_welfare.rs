//! 한국사회보장정보원 중앙부처 복지서비스 V001 API client
//!
//! API: http://apis.data.go.kr/B554287/NationalWelfareInformationsV001/NationalWelfarelistV001
//! Env: NATIONAL_WELFARE_API_KEY
//! Required params: serviceKey, callTp=L, pageNo, numOfRows, srchKeyCode=001
//!
//! 2025-04-24 공공데이터포털 변경 공지 기준 V001 엔드포인트 사용.
//! Response is XML with `<wantedList>` root.
//! Pagination: pageNo (1-based), numOfRows, totalCount in response header.

use anyhow::{Context, Result};
use chrono::Datelike;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

use super::{
    content_hash, effective_page_size, ensure_complete, http, life_stage_includes_youth,
    DataSource, RawRecord, SourceFetch,
};

const BASE_URL: &str =
    "https://apis.data.go.kr/B554287/NationalWelfareInformationsV001/NationalWelfarelistV001";
/// The service's detail (data.go.kr 15090532, `callTp=D`). Its `applmetList`
/// names the institution of each stage of the 복지 process (신청, 조사,
/// 결정, 이의신청, 지급, 사후관리) in the source's own sentence.
const DETAIL_URL: &str =
    "https://apis.data.go.kr/B554287/NationalWelfareInformationsV001/NationalWelfaredetailedV001";
/// Payload key of the kept detail. Side data: it is attached after the
/// record's hash and is one of the source's `volatile_keys`, so adding or
/// changing it never re-normalizes a programme or re-enqueues AI extraction.
pub const DETAIL_KEY: &str = "detail";
/// The whole detail read (one call per kept record) stops after this long.
const DETAIL_READ_LIMIT: Duration = Duration::from_secs(180);
/// Pause between detail calls; the detail API has its own daily quota.
const DETAIL_DELAY: Duration = Duration::from_millis(300);
const DETAIL_TEXT_MAX_CHARS: usize = 500;
/// 지원대상 (`tgtrDtlCn`) and 선정기준 (`slctCritCn`) are kept up to this
/// long: the text a review or the rule extraction reads.
const DETAIL_CONDITION_MAX_CHARS: usize = 4000;
const DETAIL_MAX_ENTRIES: usize = 60;
const PAGE_SIZE: u32 = 100;
const MAX_PAGES: u32 = 200;

pub struct NationalWelfareSource {
    client: Client,
    api_key: String,
}

impl NationalWelfareSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::http::key_in_query_client(Some(super::http::public_api_user_agent())),
            api_key,
        }
    }

    /// Returns `None` when `NATIONAL_WELFARE_API_KEY` is not set.
    pub fn from_env() -> Option<Self> {
        std::env::var("NATIONAL_WELFARE_API_KEY")
            .ok()
            .map(Self::new)
    }

    async fn fetch_page(&self, page: u32) -> Result<NationalWelfareResponse> {
        let response = http::send(
            self.client.get(BASE_URL).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("callTp", "L"),
                ("pageNo", &page.to_string()),
                ("numOfRows", &PAGE_SIZE.to_string()),
                ("srchKeyCode", "001"),
            ]),
            "national_welfare",
        )
        .await?;
        let text = http::text(response, "national_welfare").await?;

        let parsed = quick_xml::de::from_str::<NationalWelfareResponse>(&text)
            .with_context(|| format!("national_welfare: XML schema changed at page {page}"))?;
        parsed.validate()?;
        Ok(parsed)
    }

    async fn fetch_detail(&self, serv_id: &str) -> Result<Option<Value>> {
        let response = http::send(
            self.client.get(DETAIL_URL).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("callTp", "D"),
                ("servId", serv_id),
            ]),
            "national_welfare",
        )
        .await?;
        Ok(detail_projection(
            &http::text(response, "national_welfare").await?,
            serv_id,
        ))
    }

    /// The kept detail of each service id, read until [`DETAIL_READ_LIMIT`]
    /// and returned as far as it got. Best effort: the first failed call (a
    /// quota answer included) ends the read, so a spent quota is not
    /// hammered, and records without a detail keep the stored one. The read
    /// starts at a different id each day, so a read that keeps stopping
    /// early still reaches every record over the days.
    async fn fetch_details(&self, ids: &[String]) -> HashMap<String, Value> {
        let mut out = HashMap::new();
        let deadline = tokio::time::Instant::now() + DETAIL_READ_LIMIT;
        let start = rotation_start(ids.len(), chrono::Utc::now().date_naive());
        for id in ids.iter().cycle().skip(start).take(ids.len()) {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    source = "national_welfare",
                    read = out.len(),
                    "detail read reached its time limit; the rest keep the stored detail"
                );
                break;
            }
            match self.fetch_detail(id).await {
                Ok(Some(detail)) => {
                    out.insert(id.clone(), detail);
                }
                // No stage list, or not this service's answer: keep the stored one.
                Ok(None) => {}
                Err(error) => {
                    warn!(source = "national_welfare", error = %error, read = out.len(), "detail read stopped; records without a detail keep the stored one");
                    break;
                }
            }
            tokio::time::sleep(DETAIL_DELAY).await;
        }
        out
    }
}

/// Where the day's detail read starts in a list of `len` ids.
fn rotation_start(len: usize, day: chrono::NaiveDate) -> usize {
    if len == 0 {
        return 0;
    }
    (day.num_days_from_ce().unsigned_abs() as usize) % len
}

/// The kept part of one detail answer: `applmetList` as `{stage, text}`
/// entries in source order. `None` (the stored detail is kept) when the
/// answer is not the requested service's detail (an error envelope has no
/// `servId`) or lists no stage: an empty answer never blanks a stored list.
pub(crate) fn detail_projection(xml: &str, requested: &str) -> Option<Value> {
    let detail = quick_xml::de::from_str::<DetailResponse>(xml).ok()?;
    if detail.serv_id.as_deref().map(str::trim) != Some(requested.trim()) {
        return None;
    }
    let entries: Vec<Value> = detail
        .applmet
        .into_iter()
        .filter_map(|entry| {
            let text = entry.text?.trim().to_owned();
            (!text.is_empty()).then(|| {
                json!({
                    "stage": entry.stage.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
                    "text": text.chars().take(DETAIL_TEXT_MAX_CHARS).collect::<String>(),
                })
            })
        })
        .take(DETAIL_MAX_ENTRIES)
        .collect();
    if entries.is_empty() {
        return None;
    }
    let mut kept = json!({ "applmetList": entries });
    // Where 복지로 publishes who is eligible (audit M10): side data, outside
    // the record hash, for review and replay.
    for (key, text) in [
        ("tgtrDtlCn", detail.target),
        ("slctCritCn", detail.criteria),
    ] {
        if let Some(text) = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
            kept[key] = json!(text
                .chars()
                .take(DETAIL_CONDITION_MAX_CHARS)
                .collect::<String>());
        }
    }
    Some(kept)
}

#[derive(Debug, Deserialize)]
struct DetailResponse {
    #[serde(rename = "servId", default)]
    serv_id: Option<String>,
    #[serde(rename = "applmetList", default)]
    applmet: Vec<DetailEntry>,
    #[serde(rename = "tgtrDtlCn", default)]
    target: Option<String>,
    #[serde(rename = "slctCritCn", default)]
    criteria: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DetailEntry {
    #[serde(rename = "servSeDetailNm", default)]
    stage: Option<String>,
    #[serde(rename = "servSeDetailLink", default)]
    text: Option<String>,
}

impl DataSource for NationalWelfareSource {
    fn name(&self) -> &'static str {
        "national_welfare"
    }

    async fn fetch_all(&self) -> Result<SourceFetch> {
        let mut records = Vec::new();
        let mut page = 1u32;
        let mut total_filtered_out = 0usize;
        let mut collected = 0usize;
        let mut reported_total: Option<u64> = None;

        loop {
            info!(source = self.name(), page, "fetching page");

            let response = self.fetch_page(page).await?;

            let items = response.serv_list.unwrap_or_default();
            if items.is_empty() {
                info!(
                    source = self.name(),
                    page, "empty list — stopping pagination"
                );
                break;
            }

            let fetched = items.len();
            collected += fetched;
            let mut filtered_out = 0usize;
            for item in &items {
                // 청년(youth) 대상 필터: 노인/영유아/장애인 전용 등 비청년 복지가
                // 추천 풀에 들어가지 않도록 lifeArray 기준으로 걸러낸다.
                // See `is_youth_national_welfare` for the exact rule + measured
                // false-negative rate.
                if !is_youth_national_welfare(item) {
                    filtered_out += 1;
                    continue;
                }

                let payload = item_payload(item);
                let hash = content_hash(&payload);
                let source_id = crate::sources::welfare_source_id(
                    "national_welfare",
                    item.serv_id.as_deref(),
                    &hash,
                );

                records.push(RawRecord {
                    source_id,
                    payload,
                    content_hash: hash,
                });
            }
            total_filtered_out += filtered_out;

            reported_total = Some(response.total_count);
            let page_size = effective_page_size(response.num_of_rows, fetched, PAGE_SIZE);
            if (fetched as u64) < page_size || u64::from(page) * page_size >= response.total_count {
                break;
            }
            anyhow::ensure!(
                page < MAX_PAGES,
                "national_welfare: pagination limit exceeded"
            );
            page += 1;

            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        // After every hash: the detail is side data (see DETAIL_KEY).
        let ids: Vec<String> = records
            .iter()
            .filter_map(|r| {
                r.payload
                    .get("servId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let details = self.fetch_details(&ids).await;
        info!(
            source = self.name(),
            details = details.len(),
            "service details fetched"
        );
        for record in &mut records {
            let id = record
                .payload
                .get("servId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let (Some(detail), Some(object)) = (
                id.and_then(|id| details.get(&id)),
                record.payload.as_object_mut(),
            ) {
                object.insert(DETAIL_KEY.to_string(), detail.clone());
            }
        }

        let complete = ensure_complete(self.name(), collected, reported_total);
        info!(
            source = self.name(),
            total = records.len(),
            filtered_out_non_youth = total_filtered_out,
            complete,
            "fetch complete"
        );
        Ok(SourceFetch::filtered(records, collected, complete))
    }
}

// ── XML response shapes (V001) ───────────────────────────────────────────────
//
// <wantedList>
//   <totalCount>370</totalCount>
//   <pageNo>1</pageNo>
//   <numOfRows>100</numOfRows>
//   <resultCode>0</resultCode>
//   <resultMessage>SUCCESS</resultMessage>
//   <servList>
//     <servId>WLF00000022</servId>
//     <servNm>서비스명</servNm>
//     <servDgst>서비스 요약</servDgst>
//     <jurMnofNm>주관부처</jurMnofNm>
//     <jurOrgNm>주관기관</jurOrgNm>
//     <srvPvsnNm>제공유형</srvPvsnNm>
//     <sprtCycNm>지원주기</sprtCycNm>
//     <intrsThemaArray>관심테마</intrsThemaArray>
//     <trgterIndvdlArray>지원대상</trgterIndvdlArray>
//     <lifeArray>생애주기</lifeArray>
//     <onapPsbltYn>온라인신청가능여부</onapPsbltYn>
//     <rprsCtadr>대표연락처</rprsCtadr>
//     <servDtlLink>https://www.bokjiro.go.kr/...</servDtlLink>
//     <inqNum>12345</inqNum>
//     <svcfrstRegTs>20210903</svcfrstRegTs>
//   </servList>
//   ...
// </wantedList>

#[derive(Debug, Deserialize)]
#[serde(rename = "wantedList")]
struct NationalWelfareResponse {
    #[serde(rename = "resultCode")]
    result_code: String,
    #[serde(rename = "totalCount")]
    total_count: u64,
    /// Rows per page the provider actually served; may be below the request.
    #[serde(rename = "numOfRows", default)]
    num_of_rows: Option<u64>,
    #[serde(rename = "servList", default)]
    serv_list: Option<Vec<NationalWelfareItem>>,
}

impl NationalWelfareResponse {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            matches!(self.result_code.as_str(), "0" | "00"),
            "national_welfare: upstream rejected request"
        );
        Ok(())
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    /// Audit M10: the detail answer's 지원대상 and 선정기준 text, where 복지로
    /// publishes its restrictions, is kept beside the stage list (real
    /// answer, tests/fixtures/welfare_forms/national_WLF00000024.xml).
    #[test]
    fn a_detail_keeps_its_target_and_criteria_text() {
        let xml = include_str!("../../tests/fixtures/welfare_forms/national_WLF00000024.xml");
        let detail = detail_projection(xml, "WLF00000024").unwrap();
        assert!(detail["applmetList"]
            .as_array()
            .is_some_and(|l| !l.is_empty()));
        assert!(detail["tgtrDtlCn"]
            .as_str()
            .unwrap()
            .starts_with("정부 지원 대상자"));
        assert!(detail["slctCritCn"]
            .as_str()
            .unwrap()
            .starts_with("대상아동의 연령"));
        assert!(
            detail["tgtrDtlCn"].as_str().unwrap().chars().count() <= DETAIL_CONDITION_MAX_CHARS
        );
    }

    /// Audit M10: a list field the struct does not name is kept in the
    /// stored payload for replay; the known fields keep their types, so the
    /// payload (and its hash) of an item without new fields is unchanged.
    #[test]
    fn new_official_fields_are_retained_for_replay() {
        let page = "<wantedList><totalCount>1</totalCount><numOfRows>100</numOfRows><resultCode>0</resultCode><servList><servId>WLF00000022</servId><servNm>청년 지원</servNm><jurMnofNm>국토교통부</jurMnofNm><inqNum>12345</inqNum><sprtTrgtGndrCd>F</sprtTrgtGndrCd></servList></wantedList>";
        let parsed: NationalWelfareResponse = quick_xml::de::from_str(page).unwrap();
        let item = &parsed.serv_list.unwrap()[0];
        let payload = item_payload(item);
        assert_eq!(payload["sprtTrgtGndrCd"], "F");
        assert_eq!(payload["inqNum"], 12345);
        assert_eq!(payload["servNm"], "청년 지원");
        let plain = "<wantedList><totalCount>1</totalCount><resultCode>0</resultCode><servList><servId>WLF1</servId><inqNum>7</inqNum></servList></wantedList>";
        let parsed: NationalWelfareResponse = quick_xml::de::from_str(plain).unwrap();
        let payload = item_payload(&parsed.serv_list.unwrap()[0]);
        assert_eq!(payload.as_object().unwrap().len(), 16);
    }

    /// 공공분양 (WLF, NationalWelfaredetailedV001, 2026-10-31), abridged to its
    /// applmetList and a sibling list.
    const DETAIL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><wantedDtl><servId>WLF00001071</servId><servNm>공공분양</servNm><rprsCtadr>1600-1004</rprsCtadr><applmetList><servSeCode>070</servSeCode><servSeDetailLink>거주지 읍/면/동 주민센터, 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 ‘서비스 신청’</servSeDetailLink><servSeDetailNm>신청기관연락처목록</servSeDetailNm></applmetList><applmetList><servSeCode>070</servSeCode><servSeDetailLink>담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 조사 및 심사</servSeDetailLink><servSeDetailNm>조사기관연락처목록 </servSeDetailNm></applmetList><inqplCtadrList><servSeCode>010</servSeCode><servSeDetailLink>1600-1004</servSeDetailLink><servSeDetailNm>마이홈콜센터</servSeDetailNm></inqplCtadrList><resultCode>0</resultCode></wantedDtl>";

    #[test]
    fn a_detail_answer_keeps_its_stage_list_in_source_order() {
        let detail = detail_projection(DETAIL, "WLF00001071").unwrap();
        assert_eq!(
            detail,
            json!({"applmetList": [
                {"stage": "신청기관연락처목록", "text": "거주지 읍/면/동 주민센터, 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 ‘서비스 신청’"},
                {"stage": "조사기관연락처목록", "text": "담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 조사 및 심사"}
            ]})
        );
    }

    /// The daily-quota answer (HTTP 429 body) and any envelope without a
    /// service id are not an empty detail: they must never blank a stored one.
    #[test]
    fn an_error_envelope_is_not_a_detail() {
        assert_eq!(detail_projection("<OpenAPI_ServiceResponse><cmmMsgHeader><errMsg>LIMITED_NUMBER_OF_SERVICE_REQUESTS_EXCEEDS_ERROR</errMsg><returnReasonCode>22</returnReasonCode></cmmMsgHeader></OpenAPI_ServiceResponse>", "WLF00001071"), None);
        assert_eq!(detail_projection("not xml", "WLF00001071"), None);
        // Another service's answer, or one without a stage list, never
        // replaces the stored detail.
        assert_eq!(detail_projection(DETAIL, "WLF00000031"), None);
        assert_eq!(
            detail_projection(
                "<wantedDtl><servId>WLF00001071</servId><servNm>공공분양</servNm></wantedDtl>",
                "WLF00001071"
            ),
            None
        );
    }

    #[test]
    fn the_daily_read_start_moves_across_the_list() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
        let starts: std::collections::BTreeSet<usize> = (0..93)
            .map(|d| rotation_start(93, day + chrono::Days::new(d)))
            .collect();
        assert_eq!(starts.len(), 93, "every id is first on some day");
        assert_eq!(rotation_start(0, day), 0);
    }
    #[test]
    fn error_envelope_is_not_an_empty_success() {
        let response: NationalWelfareResponse = quick_xml::de::from_str(
            "<wantedList><resultCode>30</resultCode><totalCount>0</totalCount></wantedList>",
        )
        .unwrap();
        assert!(response.validate().is_err());
        assert!(quick_xml::de::from_str::<NationalWelfareResponse>("<OpenAPI_ServiceResponse><cmmMsgHeader><returnReasonCode>30</returnReasonCode></cmmMsgHeader></OpenAPI_ServiceResponse>").is_err());
        let empty: NationalWelfareResponse = quick_xml::de::from_str(
            "<wantedList><resultCode>0</resultCode><totalCount>0</totalCount></wantedList>",
        )
        .unwrap();
        assert!(empty.validate().is_ok());
    }
}

/// The stored payload of one list item: the known fields, plus every field
/// the provider sends that the struct does not name (audit M10), so a new
/// restrictive field is kept for replay instead of dropped.
fn item_payload(item: &NationalWelfareItem) -> Value {
    let mut payload = json!({
        "servId":             item.serv_id,
        "servNm":             item.serv_nm,
        "servDgst":           item.serv_dgst,
        "jurMnofNm":          item.jur_mnof_nm,
        "jurOrgNm":           item.jur_org_nm,
        "srvPvsnNm":          item.srv_pvsn_nm,
        "sprtCycNm":          item.sprt_cyc_nm,
        "intrsThemaArray":    item.intrs_thema_array,
        "trgterIndvdlArray":  item.trgter_indvdl_array,
        "lifeArray":          item.life_array,
        "onapPsbltYn":        item.onap_psblt_yn,
        "rprsCtadr":          item.rprs_ctadr,
        "servDtlLink":        item.serv_dtl_link,
        "inqNum":             item.inq_num,
        "svcfrstRegTs":       item.svc_frst_reg_ts,
        "source":             "national_welfare",
    });
    if let Some(object) = payload.as_object_mut() {
        for (key, value) in &item.extra {
            object
                .entry(key.clone())
                .or_insert_with(|| crate::sources::xml_text(value));
        }
    }
    payload
}

#[derive(Debug, Deserialize)]
struct NationalWelfareItem {
    #[serde(rename = "servId")]
    serv_id: Option<String>,
    #[serde(rename = "servNm")]
    serv_nm: Option<String>,
    #[serde(rename = "servDgst")]
    serv_dgst: Option<String>,
    /// V001: jurMnofNm (주관부처, 구 bizChrDeptNm 대체)
    #[serde(rename = "jurMnofNm")]
    jur_mnof_nm: Option<String>,
    /// V001: jurOrgNm (주관기관)
    #[serde(rename = "jurOrgNm")]
    jur_org_nm: Option<String>,
    #[serde(rename = "srvPvsnNm")]
    srv_pvsn_nm: Option<String>,
    #[serde(rename = "sprtCycNm")]
    sprt_cyc_nm: Option<String>,
    /// V001: intrsThemaArray (구 intrsThemaNmArray)
    #[serde(rename = "intrsThemaArray")]
    intrs_thema_array: Option<String>,
    /// V001: trgterIndvdlArray (구 trgterIndvdlNmArray)
    #[serde(rename = "trgterIndvdlArray")]
    trgter_indvdl_array: Option<String>,
    /// V001: 생애주기 (영유아, 아동, 청소년 등)
    #[serde(rename = "lifeArray")]
    life_array: Option<String>,
    /// V001: 온라인 신청 가능 여부 (Y/N)
    #[serde(rename = "onapPsbltYn")]
    onap_psblt_yn: Option<String>,
    /// V001: 대표 연락처
    #[serde(rename = "rprsCtadr")]
    rprs_ctadr: Option<String>,
    #[serde(rename = "servDtlLink")]
    serv_dtl_link: Option<String>,
    #[serde(rename = "inqNum")]
    inq_num: Option<u64>,
    /// V001: 최초 등록일 (구 lastModYmd 대체)
    #[serde(rename = "svcfrstRegTs")]
    svc_frst_reg_ts: Option<String>,
    /// Fields the provider sends that are not named above, as the XML
    /// reader gives them (an element's text under `$text`).
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

// ── Normalization (called from pipeline.rs) ───────────────────────────────────

pub fn normalize_national_welfare(p: &serde_json::Value) -> crate::pipeline::NormalizedProgram {
    let title = p["servNm"].as_str().unwrap_or("Unknown").to_string();
    let summary = p["servDgst"].as_str().map(|s| s.to_string());
    let provider_name = p["jurMnofNm"].as_str().map(|s| s.to_string());
    let official_url = p["servDtlLink"].as_str().map(|s| s.to_string());

    let program_type = if crate::pipeline::is_financial_support_title(&title) {
        "financial_product".into()
    } else {
        infer_national_welfare_type(
            p["intrsThemaArray"].as_str(),
            p["trgterIndvdlArray"].as_str(),
            p["srvPvsnNm"].as_str(),
        )
    };

    crate::pipeline::NormalizedProgram {
        program_type: program_type.clone(),
        title,
        summary,
        provider_name,
        company_name: None,
        benefit_category: Some(program_type.clone()),
        provider_kind: Some("government".into()),
        official_url,
        application_start_at: None,
        application_end_at: None,
        min_age: None,
        max_age: None,
        regions: vec![],
        application_method: None,
        submission_documents: None,
        screening_method: None,
        source_status: None,
        ..Default::default()
    }
}

fn infer_national_welfare_type(
    themes: Option<&str>,
    targets: Option<&str>,
    provision: Option<&str>,
) -> String {
    let combined = format!(
        "{} {} {}",
        themes.unwrap_or(""),
        targets.unwrap_or(""),
        provision.unwrap_or("")
    )
    .to_lowercase();

    if combined.contains("장학") || combined.contains("학자금") {
        "scholarship".into()
    } else if combined.contains("주거") || combined.contains("임대") || combined.contains("전세")
    {
        "housing".into()
    } else if combined.contains("취업") || combined.contains("창업") || combined.contains("고용")
    {
        "employment".into()
    } else {
        "welfare".into()
    }
}

// ── Youth (청년) filter ───────────────────────────────────────────────────────
//
// 중앙부처 복지서비스는 노인/영유아/장애인 전용 등 청년과 무관한 복지가 대부분
// (2026-07 표본 460건 중 lifeArray가 "청년"을 포함한 건 164건 = 35.7%)이라
// 필터 없이는 추천 풀이 비청년 복지로 오염된다.
//
// Primary signal: `lifeArray` (생애주기, comma-separated: 영유아/아동/청소년/
// 청년/중장년/노년/임신·출산). Present on 307/460 표본 records.
//
// Fallback: 153/460 표본 records have no `lifeArray` at all. Manually checked
// 2026-07 — 0/153 of those mention "청년" anywhere in servNm/trgterIndvdlArray/
// intrsThemaArray/servDgst either, so the keyword fallback below is a safety
// net for future API data (e.g. a new record momentarily missing lifeArray),
// not a currently-active path. This is the conservative side of the
// trade-off: precision (keep non-youth benefits like 장애인/저소득 전용
// programs out) over recall (risk of missing a genuine youth program that
// forgot to tag lifeArray) — acceptable because the measured miss rate is 0%.
fn is_youth_national_welfare(item: &NationalWelfareItem) -> bool {
    if life_stage_includes_youth(item.life_array.as_deref()) {
        return true;
    }

    // Only fall back to keyword matching when lifeArray is entirely absent
    // (not merely present-but-not-containing-청년, which is a real signal
    // that this program targets other life stages).
    if item.life_array.is_none() {
        let combined = format!(
            "{} {} {} {}",
            item.serv_nm.as_deref().unwrap_or(""),
            item.trgter_indvdl_array.as_deref().unwrap_or(""),
            item.intrs_thema_array.as_deref().unwrap_or(""),
            item.serv_dgst.as_deref().unwrap_or(""),
        );
        return combined.contains("청년");
    }

    false
}

#[cfg(test)]
mod youth_filter_tests {
    use super::*;

    fn item(
        life_array: Option<&str>,
        serv_nm: Option<&str>,
        trgter: Option<&str>,
        thema: Option<&str>,
        dgst: Option<&str>,
    ) -> NationalWelfareItem {
        NationalWelfareItem {
            serv_id: Some("WLF00000001".into()),
            serv_nm: serv_nm.map(String::from),
            serv_dgst: dgst.map(String::from),
            jur_mnof_nm: None,
            jur_org_nm: None,
            srv_pvsn_nm: None,
            sprt_cyc_nm: None,
            intrs_thema_array: thema.map(String::from),
            trgter_indvdl_array: trgter.map(String::from),
            life_array: life_array.map(String::from),
            onap_psblt_yn: None,
            rprs_ctadr: None,
            serv_dtl_link: None,
            inq_num: None,
            svc_frst_reg_ts: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn life_array_with_youth_passes() {
        let it = item(Some("청년,중장년,노년"), None, None, None, None);
        assert!(is_youth_national_welfare(&it));
    }

    #[test]
    fn life_array_without_youth_is_excluded() {
        let it = item(Some("영유아,아동,청소년"), None, None, None, None);
        assert!(!is_youth_national_welfare(&it));
    }

    #[test]
    fn life_array_elderly_only_is_excluded() {
        let it = item(
            Some("노년"),
            Some("노인일자리지원"),
            None,
            Some("일자리"),
            None,
        );
        assert!(!is_youth_national_welfare(&it));
    }

    #[test]
    fn missing_life_array_falls_back_to_keyword_and_passes() {
        let it = item(None, Some("청년 자립지원 사업"), None, None, None);
        assert!(is_youth_national_welfare(&it));
    }

    #[test]
    fn missing_life_array_falls_back_to_keyword_and_excludes() {
        // Mirrors the real 2026-07 sample: disability/housing programs with no
        // lifeArray and no mention of 청년 anywhere.
        let it = item(
            None,
            Some("장애인거주시설 이용"),
            Some("장애인"),
            Some("신체건강,생활지원,안전·위기,보호·돌봄"),
            None,
        );
        assert!(!is_youth_national_welfare(&it));
    }

    #[test]
    fn present_but_non_youth_life_array_does_not_use_keyword_fallback() {
        // lifeArray IS present (just doesn't include 청년) — this must NOT
        // fall through to keyword matching even if 청년 appears elsewhere,
        // because an explicit life-stage tag is a stronger signal than a
        // stray keyword mention (e.g. "청년" mentioned in passing in a
        // 노년-targeted summary's caregiver context).
        let it = item(
            Some("노년"),
            Some("노인 돌봄 서비스"),
            None,
            None,
            Some("자녀나 청년 가족이 신청을 대행할 수 있습니다"),
        );
        assert!(!is_youth_national_welfare(&it));
    }
}
