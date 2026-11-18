//! 온통청년(청년정책) Open API client
//!
//! API: https://www.youthcenter.go.kr/go/ythip/getPlcy
//! Env: YOUTH_CENTER_API_KEY  (온통청년 Open API 인증키)
//!
//! Response format: JSON (2026년 신규 엔드포인트)
//! Pagination params: pageNum (1-based), pageSize

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::info;

use super::{
    content_hash, effective_page_size, ensure_complete, http, DataSource, RawRecord, SourceFetch,
};

pub(crate) const BASE_URL: &str = "https://www.youthcenter.go.kr/go/ythip/getPlcy";
const PAGE_SIZE: u32 = 100;
const MAX_PAGES: u32 = 1000;

pub struct YouthCenterSource {
    client: Client,
    api_key: String,
}

impl YouthCenterSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::http::key_in_query_client(Some(super::http::public_api_user_agent())),
            api_key,
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("YOUTH_CENTER_API_KEY").context("YOUTH_CENTER_API_KEY not set")?;
        Ok(Self::new(key))
    }

    async fn fetch_page(&self, page: u32) -> Result<YouthCenterResponse> {
        let response = http::send(
            self.client.get(BASE_URL).query(&[
                ("apiKeyNm", self.api_key.as_str()),
                ("pageNum", &page.to_string()),
                ("pageSize", &PAGE_SIZE.to_string()),
                ("rtnType", "json"),
                ("pageType", "1"),
            ]),
            "youth_center",
        )
        .await?;
        let text = http::text(response, "youth_center").await?;

        let parsed = serde_json::from_str::<YouthCenterResponse>(&text)
            .with_context(|| format!("youth_center: JSON schema changed at page {page}"))?;
        parsed.validate()?;
        Ok(parsed)
    }
}

impl DataSource for YouthCenterSource {
    fn name(&self) -> &'static str {
        "youth_center"
    }

    async fn fetch_all(&self) -> Result<SourceFetch> {
        let mut records = Vec::new();
        let mut page = 1u32;
        let mut reported_total: Option<u64> = None;

        loop {
            info!(source = self.name(), page, "fetching page");

            let resp = self.fetch_page(page).await?;

            let items = resp
                .result
                .as_ref()
                .map(|r| &r.youth_policy_list)
                .cloned()
                .unwrap_or_default();

            if items.is_empty() {
                info!(
                    source = self.name(),
                    page, "empty youthPolicyList — stopping pagination"
                );
                break;
            }

            let fetched = items.len();
            for item in items {
                let payload = serde_json::to_value(&item).unwrap_or(Value::Null);
                let hash = content_hash(&payload);
                // A record without an upstream id gets a content-derived id,
                // never a position-derived one: positions shift between runs
                // and would make lifecycle reconciliation close and recreate
                // the same program every day.
                let source_id = item
                    .plcy_no
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("yc_unknown_{}", &hash[..16]));

                records.push(RawRecord {
                    source_id,
                    payload,
                    content_hash: hash,
                });
            }

            let pagging = resp.result.as_ref().and_then(|r| r.pagging.as_ref());
            reported_total = pagging.map(|p| p.tot_count).filter(|total| *total > 0);
            let page_size =
                effective_page_size(pagging.and_then(|p| p.page_size), fetched, PAGE_SIZE);
            let last_page = (fetched as u64) < page_size
                || reported_total.is_some_and(|total| u64::from(page) * page_size >= total);
            if last_page {
                break;
            }
            anyhow::ensure!(page < MAX_PAGES, "youth_center: pagination limit exceeded");
            page += 1;

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        let complete = ensure_complete(self.name(), records.len(), reported_total);
        info!(
            source = self.name(),
            total = records.len(),
            complete,
            "fetch complete"
        );
        Ok(SourceFetch::unfiltered(records, complete))
    }
}

// ── JSON response shapes (2026 new endpoint) ────────────────────────────────
//
// {
//   "resultCode": 200,
//   "resultMessage": "성공적으로 데이터를 가지고 왔습니다.",
//   "result": {
//     "pagging": { "totCount": 1828, "pageNum": 1, "pageSize": 100 },
//     "youthPolicyList": [ { ... }, ... ]
//   }
// }

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouthCenterResponse {
    result_code: u32,
    #[allow(dead_code)]
    result_message: Option<String>,
    result: Option<YouthCenterResult>,
}

impl YouthCenterResponse {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.result_code == 200,
            "youth_center: upstream rejected request (code {})",
            self.result_code
        );
        anyhow::ensure!(self.result.is_some(), "youth_center: missing result");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YouthCenterResult {
    pagging: Option<Pagging>,
    #[serde(default)]
    youth_policy_list: Vec<YouthPolicyItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pagging {
    tot_count: u64,
    /// Rows per page the provider actually served; may be below the request.
    page_size: Option<u64>,
}

#[allow(non_snake_case)]
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YouthPolicyItem {
    /// 정책 번호 (고유키)
    pub plcy_no: Option<String>,
    /// 정책명
    pub plcy_nm: Option<String>,
    /// 정책 설명
    pub plcy_expln_cn: Option<String>,
    /// 정책 지원 내용
    pub plcy_sprt_cn: Option<String>,
    /// 정책 키워드
    pub plcy_kywd_nm: Option<String>,
    /// 대분류명
    pub lclsf_nm: Option<String>,
    /// 중분류명
    pub mclsf_nm: Option<String>,
    /// 주관기관 코드명
    pub sprvsn_inst_cd_nm: Option<String>,
    /// 운영기관 코드명
    pub oper_inst_cd_nm: Option<String>,
    /// 신청 URL
    pub aply_url_addr: Option<String>,
    /// 사업기간 시작일 (YYYYMMDD)
    pub biz_prd_bgng_ymd: Option<String>,
    /// 사업기간 종료일 (YYYYMMDD)
    pub biz_prd_end_ymd: Option<String>,
    /// 신청기간 (텍스트, "YYYYMMDD ~ YYYYMMDD")
    pub aply_ymd: Option<String>,
    /// 지원대상 최소나이
    pub sprt_trgt_min_age: Option<String>,
    /// 지원대상 최대나이
    pub sprt_trgt_max_age: Option<String>,
    /// 나이제한 여부 (Y/N)
    pub sprt_trgt_age_lmt_yn: Option<String>,
    /// 정책 거주지역 법정 시군구코드 (콤마 구분, 일반 우편번호가 아님)
    pub zip_cd: Option<String>,
    /// 참고 URL 1
    pub ref_url_addr1: Option<String>,
    /// 참고 URL 2
    pub ref_url_addr2: Option<String>,
    /// 정책 대분류 코드
    pub plcy_major_cd: Option<String>,
    /// 소규모 사업 코드
    pub sbiz_cd: Option<String>,
    /// 조회수 (API returns string)
    pub inq_cnt: Option<String>,
    /// 신청 방법 (신청 절차 안내 텍스트)
    pub plcy_aply_mthd_cn: Option<String>,
    /// 제출 서류 안내 텍스트
    pub sbmsn_dcmnt_cn: Option<String>,
    /// 심사 방법 안내 텍스트
    pub srng_mthd_cn: Option<String>,
    /// 추가 자격 조건 텍스트 (raw_payload에만 보존, 별도 DB 컬럼 없음)
    pub add_aply_qlfc_cnd_cn: Option<String>,
    /// Keep newly introduced official fields in snapshots for later replay.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_official_fields_are_retained_for_replay() {
        let item: YouthPolicyItem = serde_json::from_value(serde_json::json!({
            "plcyNo":"fixture", "zipCd":"41461", "futureResidenceField":{"code":"41"}
        }))
        .unwrap();
        let saved = serde_json::to_value(item).unwrap();
        assert_eq!(saved["zipCd"], "41461");
        assert_eq!(saved["futureResidenceField"]["code"], "41");
    }

    #[test]
    fn provider_error_or_missing_result_fails_collection() {
        for body in [
            r#"{"resultCode":400,"resultMessage":"bad request","result":null}"#,
            r#"{"resultCode":200,"result":null}"#,
        ] {
            let parsed: YouthCenterResponse = serde_json::from_str(body).unwrap();
            assert!(parsed.validate().is_err());
        }
        let empty: YouthCenterResponse =
            serde_json::from_str(r#"{"resultCode":200,"result":{"youthPolicyList":[]}}"#).unwrap();
        assert!(empty.validate().is_ok());
    }

    /// Real 2026-10-12 response page (three items, envelope as served). The
    /// provider's own typo `aplyYmd: "20260601 ~ 20250710"` must survive the
    /// fetch layer untouched; it is the normalizer's job to reject it.
    #[test]
    fn real_page_with_inverted_application_window_parses() {
        let parsed: YouthCenterResponse = serde_json::from_str(include_str!(
            "../../tests/fixtures/youth_center_page_inverted_window.json"
        ))
        .unwrap();
        parsed.validate().unwrap();
        let list = parsed.result.unwrap().youth_policy_list;
        assert_eq!(list.len(), 3);
        let typo = list
            .iter()
            .find(|item| item.plcy_no.as_deref() == Some("20260422005400212868"))
            .unwrap();
        assert_eq!(typo.aply_ymd.as_deref(), Some("20260601 ~ 20250710"));
        assert!(typo.extra.contains_key("aplyPrdSeCd"));
    }

    /// 실제 API 호출 통합 테스트 (YOUTH_CENTER_API_KEY 환경변수 필요)
    #[tokio::test]
    #[ignore] // cargo test -- --ignored 로 실행
    async fn live_fetch_first_page() {
        let src = YouthCenterSource::from_env().expect("YOUTH_CENTER_API_KEY required");
        let resp = src.fetch_page(1).await.expect("fetch_page failed");
        assert_eq!(
            resp.result_code, 200,
            "API returned error: {:?}",
            resp.result_message
        );
        let result = resp.result.expect("result is None");
        let total = result.pagging.as_ref().map(|p| p.tot_count).unwrap_or(0);
        assert!(total > 0, "totCount should be > 0, got {total}");
        assert!(
            !result.youth_policy_list.is_empty(),
            "youthPolicyList is empty"
        );
        let first = &result.youth_policy_list[0];
        assert!(first.plcy_no.is_some(), "plcyNo missing");
        assert!(first.plcy_nm.is_some(), "plcyNm missing");
        println!(
            "OK: total={total}, first={}",
            first.plcy_nm.as_deref().unwrap_or("?")
        );
    }
}
