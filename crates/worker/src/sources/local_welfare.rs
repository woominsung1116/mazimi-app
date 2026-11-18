//! 한국사회보장정보원 지자체 복지서비스 API client
//!
//! API: http://apis.data.go.kr/B554287/LocalGovernmentWelfareInformations/LcgvWelfarelist
//! Env: LOCAL_WELFARE_API_KEY
//!
//! Response is XML with `<wantedList>` root. No JSON mode available.
//! Confirmed working: 4559+ records as of 2026-04.
//! Pagination: pageNo (1-based), numOfRows, totalCount in response header.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tracing::info;

use super::{
    content_hash, effective_page_size, ensure_complete, http, life_stage_includes_youth,
    DataSource, RawRecord, SourceFetch,
};

const BASE_URL: &str =
    "https://apis.data.go.kr/B554287/LocalGovernmentWelfareInformations/LcgvWelfarelist";
const PAGE_SIZE: u32 = 100;
const MAX_PAGES: u32 = 200;

pub struct LocalWelfareSource {
    client: Client,
    api_key: String,
}

impl LocalWelfareSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::http::key_in_query_client(Some(super::http::public_api_user_agent())),
            api_key,
        }
    }

    /// Returns `None` when `LOCAL_WELFARE_API_KEY` is not set.
    pub fn from_env() -> Option<Self> {
        std::env::var("LOCAL_WELFARE_API_KEY").ok().map(Self::new)
    }

    async fn fetch_page(&self, page: u32) -> Result<LocalWelfareResponse> {
        let response = http::send(
            self.client.get(BASE_URL).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("callTp", "L"),
                ("pageNo", &page.to_string()),
                ("numOfRows", &PAGE_SIZE.to_string()),
            ]),
            "local_welfare",
        )
        .await?;
        let text = http::text(response, "local_welfare").await?;

        let parsed = quick_xml::de::from_str::<LocalWelfareResponse>(&text)
            .with_context(|| format!("local_welfare: XML schema changed at page {page}"))?;
        parsed.validate()?;
        Ok(parsed)
    }
}

impl DataSource for LocalWelfareSource {
    fn name(&self) -> &'static str {
        "local_welfare"
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
                // 청년(youth) 대상 필터: lifeNmArray 기준. 지자체 복지는 노인
                // 대상 복지 비중이 매우 높아(4,600건 중 lifeNmArray에 "청년"
                // 포함 건 1,202건 = 26.1%) 필터 없이는 추천 풀 대부분이
                // 비청년 복지로 채워진다. See `is_youth_local_welfare`.
                if !is_youth_local_welfare(item) {
                    filtered_out += 1;
                    continue;
                }

                let payload = item_payload(item);
                let hash = content_hash(&payload);
                let source_id = crate::sources::welfare_source_id(
                    "local_welfare",
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
            anyhow::ensure!(page < MAX_PAGES, "local_welfare: pagination limit exceeded");
            page += 1;

            tokio::time::sleep(Duration::from_millis(300)).await;
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

// ── XML response shapes ───────────────────────────────────────────────────────
//
// <wantedList>
//   <totalCount>4559</totalCount>
//   <pageNo>1</pageNo>
//   <numOfRows>100</numOfRows>
//   <resultCode>0</resultCode>
//   <resultMessage>SUCCESS</resultMessage>
//   <servList>
//     <servId>WLF00004882</servId>
//     <servNm>무주추모의집 운영</servNm>
//     <servDgst>무주추모의집을 운영함으로써...</servDgst>
//     <bizChrDeptNm>전북특별자치도 무주군 행정복지국 사회복지과</bizChrDeptNm>
//     <ctpvNm>전북특별자치도</ctpvNm>
//     <sggNm>무주군</sggNm>
//     <srvPvsnNm>기타</srvPvsnNm>
//     <sprtCycNm>1회성</sprtCycNm>
//     <aplyMtdNm>방문</aplyMtdNm>
//     <intrsThemaNmArray>안전·위기</intrsThemaNmArray>
//     <trgterIndvdlNmArray>장애인</trgterIndvdlNmArray>
//     <lifeNmArray>노년</lifeNmArray>
//     <servDtlLink>https://www.bokjiro.go.kr/...</servDtlLink>
//     <inqNum>1241</inqNum>
//     <lastModYmd>20260405</lastModYmd>
//   </servList>
//   ...
// </wantedList>

#[derive(Debug, Deserialize)]
#[serde(rename = "wantedList")]
struct LocalWelfareResponse {
    #[serde(rename = "resultCode")]
    result_code: String,
    #[serde(rename = "totalCount")]
    total_count: u64,
    /// Rows per page the provider actually served; may be below the request.
    #[serde(rename = "numOfRows", default)]
    num_of_rows: Option<u64>,
    #[serde(rename = "servList", default)]
    serv_list: Option<Vec<LocalWelfareItem>>,
}

impl LocalWelfareResponse {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            matches!(self.result_code.as_str(), "0" | "00"),
            "local_welfare: upstream rejected request"
        );
        Ok(())
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    /// Audit M10: a list field the struct does not name is kept in the
    /// stored payload; an item without new fields keeps its payload.
    #[test]
    fn new_official_fields_are_retained_for_replay() {
        let page = "<wantedList><totalCount>1</totalCount><resultCode>0</resultCode><servList><servId>WLF00004882</servId><servNm>청년 지원</servNm><ctpvNm>부산광역시</ctpvNm><inqNum>3</inqNum><sprtTrgtGndrCd>F</sprtTrgtGndrCd></servList></wantedList>";
        let parsed: LocalWelfareResponse = quick_xml::de::from_str(page).unwrap();
        let items = parsed.serv_list.unwrap();
        let payload = item_payload(&items[0]);
        assert_eq!(payload["sprtTrgtGndrCd"], "F");
        assert_eq!(payload["inqNum"], 3);
        assert_eq!(payload.as_object().unwrap().len(), 17);
    }

    /// Audit L1: records without a servId keep apart.
    #[test]
    fn records_without_an_id_never_share_one() {
        let a = crate::sources::welfare_source_id("local_welfare", None, "aaaaaaaaaaaaaaaaaaaa");
        let b =
            crate::sources::welfare_source_id("local_welfare", Some(" "), "bbbbbbbbbbbbbbbbbbbb");
        assert_ne!(a, b);
        assert_eq!(
            crate::sources::welfare_source_id("local_welfare", Some("WLF1"), "x"),
            "local_welfare_WLF1"
        );
    }
    #[test]
    fn error_envelope_is_not_an_empty_success() {
        let response: LocalWelfareResponse = quick_xml::de::from_str(
            "<wantedList><resultCode>30</resultCode><totalCount>0</totalCount></wantedList>",
        )
        .unwrap();
        assert!(response.validate().is_err());
        assert!(quick_xml::de::from_str::<LocalWelfareResponse>("<OpenAPI_ServiceResponse><cmmMsgHeader><returnReasonCode>30</returnReasonCode></cmmMsgHeader></OpenAPI_ServiceResponse>").is_err());
        let empty: LocalWelfareResponse = quick_xml::de::from_str(
            "<wantedList><resultCode>0</resultCode><totalCount>0</totalCount></wantedList>",
        )
        .unwrap();
        assert!(empty.validate().is_ok());
    }
}

/// The stored payload of one list item: the known fields, plus every field
/// the provider sends that the struct does not name (audit M10).
fn item_payload(item: &LocalWelfareItem) -> serde_json::Value {
    let mut payload = json!({
        "servId":         item.serv_id,
        "servNm":         item.serv_nm,
        "servDgst":       item.serv_dgst,
        "bizChrDeptNm":   item.biz_chr_dept_nm,
        "ctpvNm":         item.ctpv_nm,
        "sggNm":          item.sgg_nm,
        "srvPvsnNm":      item.srv_pvsn_nm,
        "sprtCycNm":      item.sprt_cyc_nm,
        "aplyMtdNm":      item.aply_mtd_nm,
        "intrsThemaNmArray": item.intrs_thema_nm_array,
        "trgterIndvdlNmArray": item.trgter_indvdl_nm_array,
        "lifeNmArray":    item.life_nm_array,
        "servDtlLink":    item.serv_dtl_link,
        "inqNum":         item.inq_num,
        "lastModYmd":     item.last_mod_ymd,
        "source":         "local_welfare",
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
struct LocalWelfareItem {
    #[serde(rename = "servId")]
    serv_id: Option<String>,
    #[serde(rename = "servNm")]
    serv_nm: Option<String>,
    #[serde(rename = "servDgst")]
    serv_dgst: Option<String>,
    #[serde(rename = "bizChrDeptNm")]
    biz_chr_dept_nm: Option<String>,
    #[serde(rename = "ctpvNm")]
    ctpv_nm: Option<String>,
    #[serde(rename = "sggNm")]
    sgg_nm: Option<String>,
    #[serde(rename = "srvPvsnNm")]
    srv_pvsn_nm: Option<String>,
    #[serde(rename = "sprtCycNm")]
    sprt_cyc_nm: Option<String>,
    #[serde(rename = "aplyMtdNm")]
    aply_mtd_nm: Option<String>,
    #[serde(rename = "intrsThemaNmArray")]
    intrs_thema_nm_array: Option<String>,
    #[serde(rename = "trgterIndvdlNmArray")]
    trgter_indvdl_nm_array: Option<String>,
    /// 생애주기 (영유아, 아동, 청소년, 청년, 중장년, 노년, 임신 · 출산 등).
    /// Present on ~70% of records (2026-07 표본 4,600건 중 3,205건).
    #[serde(rename = "lifeNmArray")]
    life_nm_array: Option<String>,
    #[serde(rename = "servDtlLink")]
    serv_dtl_link: Option<String>,
    #[serde(rename = "inqNum")]
    inq_num: Option<u64>,
    #[serde(rename = "lastModYmd")]
    last_mod_ymd: Option<String>,
    /// Fields the provider sends that are not named above, as the XML
    /// reader gives them (an element's text under `$text`).
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

// ── Normalization (called from pipeline.rs) ───────────────────────────────────

pub fn normalize_local_welfare(p: &serde_json::Value) -> crate::pipeline::NormalizedProgram {
    let title = p["servNm"].as_str().unwrap_or("Unknown").to_string();
    let summary = p["servDgst"].as_str().map(|s| s.to_string());
    let provider_name = p["bizChrDeptNm"].as_str().map(|s| s.to_string());
    let official_url = p["servDtlLink"].as_str().map(|s| s.to_string());

    // `programs.regions` holds canonical sido keys; the 시·군·구 named by
    // `sggNm` goes to `region_districts` only.
    let scope = crate::regions::local_welfare_scope(p);

    let program_type = if crate::pipeline::is_financial_support_title(&title) {
        "financial_product".into()
    } else {
        infer_local_welfare_type(
            p["intrsThemaNmArray"].as_str(),
            p["trgterIndvdlNmArray"].as_str(),
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
        provider_kind: Some("municipality".into()),
        official_url,
        application_start_at: None,
        application_end_at: None,
        min_age: None,
        max_age: None,
        regions: scope.regions,
        application_method: crate::pipeline::source_text(p, "aplyMtdNm"),
        submission_documents: None,
        screening_method: None,
        source_status: None,
        region_districts: scope.districts,
        scope_note: scope.note,
        ..Default::default()
    }
}

fn infer_local_welfare_type(
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
// 지자체 복지서비스는 노인 대상 복지 비중이 매우 높다 (2026-07 표본 4,600건
// 중 lifeNmArray가 "청년"을 포함한 건 1,202건 = 26.1%). 필터 없이는 추천 풀
// 대부분이 비청년(특히 노년) 복지로 채워진다.
//
// Primary signal: `lifeNmArray` (생애주기). Present on 3,205/4,600 표본
// records (~70%).
//
// Fallback: 1,395/4,600 표본 records have no `lifeNmArray` tag at all.
// Manually checked 2026-07 — 10/1,395 of those DO mention "청년" in
// servNm/trgterIndvdlNmArray/intrsThemaNmArray/servDgst (e.g. "청년 시험응시료
// 지원사업", "으뜸관악 청년통장") despite lacking the life-stage tag, so
// (unlike national_welfare where the equivalent fallback measured 0/153) this
// keyword fallback is an active, evidence-backed path here — dropping it
// would silently discard real youth programs that the API forgot to tag.
fn is_youth_local_welfare(item: &LocalWelfareItem) -> bool {
    if life_stage_includes_youth(item.life_nm_array.as_deref()) {
        return true;
    }

    // Only fall back to keyword matching when lifeNmArray is entirely absent
    // (not merely present-but-not-containing-청년).
    if item.life_nm_array.is_none() {
        let combined = format!(
            "{} {} {} {}",
            item.serv_nm.as_deref().unwrap_or(""),
            item.trgter_indvdl_nm_array.as_deref().unwrap_or(""),
            item.intrs_thema_nm_array.as_deref().unwrap_or(""),
            item.serv_dgst.as_deref().unwrap_or(""),
        );
        return combined.contains("청년");
    }

    false
}

#[cfg(test)]
mod district_tests {
    use super::*;

    fn scope(ctpv: Option<&str>, sgg: Option<&str>) -> (Vec<String>, Option<Vec<String>>) {
        let program = normalize_local_welfare(&json!({
            "servNm": "청년 지원", "ctpvNm": ctpv, "sggNm": sgg,
        }));
        (program.regions, program.region_districts)
    }

    fn codes(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|c| c.to_string()).collect())
    }

    #[test]
    fn a_district_programme_is_limited_to_its_district() {
        assert_eq!(
            scope(Some("경상남도"), Some("밀양시")),
            (vec!["gyeongnam".into()], codes(&["48270"]))
        );
        assert_eq!(
            scope(Some("부산광역시"), Some("해운대구")),
            (vec!["busan".into()], codes(&["26350"]))
        );
    }

    #[test]
    fn a_city_with_gu_is_stored_with_every_gu() {
        assert_eq!(
            scope(Some("경상남도"), Some("창원시")),
            (
                vec!["gyeongnam".into()],
                codes(&["48120", "48121", "48123", "48125", "48127", "48129"])
            )
        );
        assert_eq!(
            scope(Some("경상남도"), Some("창원시 의창구")).1,
            codes(&["48121"])
        );
    }

    #[test]
    fn merged_jeonnam_gwangju_districts_keep_their_retired_codes() {
        assert_eq!(
            scope(Some("전남광주통합특별시"), Some("북구")),
            (vec!["gwangju".into()], codes(&["12300", "29170"]))
        );
        assert_eq!(
            scope(Some("전남광주통합특별시"), Some("순천시")),
            (vec!["jeonnam".into()], codes(&["12150", "46150"]))
        );
        assert_eq!(
            scope(Some("전남광주통합특별시"), None),
            (vec!["gwangju".into(), "jeonnam".into()], None)
        );
    }

    #[test]
    fn a_sido_wide_or_unresolved_district_has_no_district_list() {
        for sgg in [None, Some(""), Some("  ")] {
            assert_eq!(
                scope(Some("경상남도"), sgg),
                (vec!["gyeongnam".into()], None),
                "{sgg:?}"
            );
        }
        // Not a district of the named sido, or no such district: never guessed.
        for sgg in ["밀양시", "없는군", "청년정책과"] {
            assert_eq!(
                scope(Some("부산광역시"), Some(sgg)),
                (vec!["busan".into()], None),
                "{sgg}"
            );
        }
        assert_eq!(
            scope(None, Some("밀양시")),
            (vec![crate::regions::UNKNOWN.into()], None)
        );
        assert_eq!(
            scope(Some("전남광주통합특별시"), Some("없는구")),
            (vec![crate::regions::UNKNOWN.into()], None)
        );
    }

    /// sggNm names one district ("창원시 의창구" is one); a list or a
    /// department after it must not narrow the scope to the first name.
    #[test]
    fn a_multi_district_or_suffixed_sgg_name_has_no_district_list() {
        for sgg in [
            "밀양시 김해시",
            "밀양시,김해시",
            "밀양시, 김해시",
            "밀양시·김해시",
            "밀양시 청년정책과",
            "창원시 의창구 성산구",
        ] {
            assert_eq!(
                scope(Some("경상남도"), Some(sgg)),
                (vec!["gyeongnam".into()], None),
                "{sgg}"
            );
        }
        // Two 구 of a 광역시 are a list, not "시 구" (review N1/N2).
        for (sido, sgg, region) in [
            ("부산광역시", "해운대구 수영구", "busan"),
            ("서울특별시", "강남구 서초구", "seoul"),
            ("경상남도", "창원시 해운대구", "gyeongnam"),
        ] {
            assert_eq!(
                scope(Some(sido), Some(sgg)),
                (vec![region.into()], None),
                "{sgg}"
            );
        }
    }
}

#[cfg(test)]
mod youth_filter_tests {
    use super::*;

    fn item(
        life_nm_array: Option<&str>,
        serv_nm: Option<&str>,
        trgter: Option<&str>,
        thema: Option<&str>,
        dgst: Option<&str>,
    ) -> LocalWelfareItem {
        LocalWelfareItem {
            serv_id: Some("WLF00000001".into()),
            serv_nm: serv_nm.map(String::from),
            serv_dgst: dgst.map(String::from),
            biz_chr_dept_nm: None,
            ctpv_nm: None,
            sgg_nm: None,
            srv_pvsn_nm: None,
            sprt_cyc_nm: None,
            aply_mtd_nm: None,
            intrs_thema_nm_array: thema.map(String::from),
            trgter_indvdl_nm_array: trgter.map(String::from),
            life_nm_array: life_nm_array.map(String::from),
            serv_dtl_link: None,
            inq_num: None,
            last_mod_ymd: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn life_nm_array_with_youth_passes() {
        let it = item(Some("청년"), None, None, None, None);
        assert!(is_youth_local_welfare(&it));
    }

    #[test]
    fn life_nm_array_with_youth_among_others_passes() {
        let it = item(Some("중장년, 노년, 청년"), None, None, None, None);
        assert!(is_youth_local_welfare(&it));
    }

    #[test]
    fn life_nm_array_elderly_only_is_excluded() {
        let it = item(
            Some("노년"),
            Some("무주추모의집 운영"),
            Some("장애인"),
            Some("안전·위기"),
            None,
        );
        assert!(!is_youth_local_welfare(&it));
    }

    #[test]
    fn missing_life_nm_array_falls_back_to_keyword_and_passes() {
        // Mirrors the real 2026-07 sample: "청년 시험응시료 지원사업" has no
        // lifeNmArray tag but clearly targets youth.
        let it = item(None, Some("청년 시험응시료 지원사업"), None, None, None);
        assert!(is_youth_local_welfare(&it));
    }

    #[test]
    fn missing_life_nm_array_falls_back_to_keyword_and_excludes() {
        let it = item(
            None,
            Some("무주군 결식아동 지원사업"),
            Some("아동"),
            None,
            None,
        );
        assert!(!is_youth_local_welfare(&it));
    }

    #[test]
    fn present_but_non_youth_life_nm_array_does_not_use_keyword_fallback() {
        let it = item(
            Some("노년"),
            Some("노인 돌봄 서비스"),
            None,
            None,
            Some("청년 자녀가 신청을 대행할 수 있습니다"),
        );
        assert!(!is_youth_local_welfare(&it));
    }
}
