//! `document_issue_link`: where to issue a required document yourself.
//!
//! A static, versioned catalog in code. Each URL is an official issuing
//! site checked to resolve (HTTP 200 over HTTPS, page title naming the
//! service or site) on `verified_on`; re-run the ignored test
//! `issuer_urls_still_resolve` before changing the catalog version. The user
//! logs in on the official site; the app never handles credentials.
//! School documents have no single site: guidance only, no link.
use super::{
    tools::{tool_error, Outcome},
    ToolError,
};
use crate::services::application_preparation;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub const CATALOG_VERSION: &str = "document-issuers-2026-10-16";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Issuer {
    pub id: &'static str,
    /// The document as the card names it.
    pub document: &'static str,
    pub issuer: &'static str,
    pub url: Option<&'static str>,
    pub guidance: &'static str,
    pub verified_on: Option<&'static str>,
}

/// Hosts a catalog link may point at; the mobile card checks the same list.
pub const ISSUER_HOSTS: &[&str] = &[
    "www.gov.kr",
    "efamily.scourt.go.kr",
    "www.nhis.or.kr",
    "hometax.go.kr",
];

const GOV24_RESIDENT: &str =
    "https://www.gov.kr/mw/AA020InfoCappView.do?HighCtgCD=A01010001&CappBizCD=13100000015";
const SCHOOL: &str = "다니는(졸업한) 학교의 포털이나 증명서 발급 사이트에서 발급해요. 학교마다 사이트가 달라 링크는 드리지 않아요.";

pub const CATALOG: &[Issuer] = &[
    Issuer {
        id: "resident_register_copy",
        document: "주민등록표 등본",
        issuer: "정부24",
        url: Some(GOV24_RESIDENT),
        guidance: "정부24에 로그인한 뒤 ‘주민등록표 등본(초본) 발급’에서 등본을 고르세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "resident_register_abstract",
        document: "주민등록표 초본",
        issuer: "정부24",
        url: Some(GOV24_RESIDENT),
        guidance: "정부24에 로그인한 뒤 ‘주민등록표 등본(초본) 발급’에서 초본을 고르세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "family_relation",
        document: "가족관계증명서",
        issuer: "대법원 전자가족관계등록시스템",
        url: Some("https://efamily.scourt.go.kr/index.jsp"),
        guidance: "전자가족관계등록시스템에서 본인 인증 후 ‘증명서 발급’으로 들어가세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "basic_certificate",
        document: "기본증명서",
        issuer: "대법원 전자가족관계등록시스템",
        url: Some("https://efamily.scourt.go.kr/index.jsp"),
        guidance: "전자가족관계등록시스템에서 본인 인증 후 ‘증명서 발급’으로 들어가세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "health_insurance_eligibility",
        document: "건강보험 자격득실확인서",
        issuer: "국민건강보험공단",
        url: Some("https://www.nhis.or.kr/"),
        guidance: "국민건강보험공단 누리집에 로그인한 뒤 증명서 발급 메뉴에서 ‘자격득실확인서’를 찾으세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "health_insurance_status",
        document: "건강보험 자격확인서",
        issuer: "국민건강보험공단",
        url: Some("https://www.nhis.or.kr/"),
        guidance: "국민건강보험공단 누리집에 로그인한 뒤 증명서 발급 메뉴에서 ‘자격확인서’를 찾으세요. 가입 이력이 필요하면 ‘자격득실확인서’를 따로 발급해야 해요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "health_insurance_payment",
        document: "건강보험료 납부확인서",
        issuer: "국민건강보험공단",
        url: Some("https://www.nhis.or.kr/"),
        guidance: "국민건강보험공단 누리집에 로그인한 뒤 증명서 발급 메뉴에서 ‘납부확인서’를 찾으세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "income_amount",
        document: "소득금액증명원",
        issuer: "국세청 홈택스",
        url: Some("https://hometax.go.kr/"),
        guidance: "홈택스에 로그인한 뒤 민원증명 메뉴에서 ‘소득금액증명’을 찾으세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "business_registration",
        document: "사업자등록증명",
        issuer: "정부24",
        url: Some("https://www.gov.kr/mw/AA020InfoCappView.do?CappBizCD=12100000016"),
        guidance: "정부24에 로그인한 뒤 ‘사업자등록증명 발급’에서 신청하세요.",
        verified_on: Some("2026-10-16"),
    },
    Issuer {
        id: "enrollment",
        document: "재학증명서",
        issuer: "다니는 학교",
        url: None,
        guidance: SCHOOL,
        verified_on: None,
    },
    Issuer {
        id: "leave",
        document: "휴학증명서",
        issuer: "다니는 학교",
        url: None,
        guidance: SCHOOL,
        verified_on: None,
    },
    Issuer {
        id: "graduation",
        document: "졸업증명서",
        issuer: "졸업한 학교",
        url: None,
        guidance: SCHOOL,
        verified_on: None,
    },
    Issuer {
        id: "transcript",
        document: "성적증명서",
        issuer: "다니는(졸업한) 학교",
        url: None,
        guidance: SCHOOL,
        verified_on: None,
    },
];

fn by_id(id: &str) -> Option<&'static Issuer> {
    CATALOG.iter().find(|i| i.id == id)
}

/// The catalog entry for a document name as notices and users write it.
/// Order matters: register copies of land, buildings or companies are not
/// resident registers.
pub fn lookup(name: &str) -> Option<&'static Issuer> {
    let n: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    let has = |s: &str| n.contains(s);
    let id = if has("가족관계") {
        "family_relation"
    } else if has("기본증명") {
        "basic_certificate"
    } else if has("등기") || has("건축물") || has("토지") || has("법인") || has("부동산")
    {
        return None;
    } else if has("주민등록") || has("주민등록표") || n.starts_with("등본") || n.starts_with("초본")
    {
        if has("초본") {
            "resident_register_abstract"
        } else if has("등본") {
            "resident_register_copy"
        } else {
            return None;
        }
    } else if has("자격득실") {
        "health_insurance_eligibility"
    } else if has("건강보험") && has("자격확인") {
        "health_insurance_status"
    } else if has("건강보험") && has("자격") {
        // Not sure which of the two: say unknown rather than guess.
        return None;
    } else if has("건강보험") && has("납부") {
        "health_insurance_payment"
    } else if has("소득금액") {
        "income_amount"
    } else if has("사업자등록") {
        "business_registration"
    } else if has("재학") && has("증명") {
        "enrollment"
    } else if has("휴학") && has("증명") {
        "leave"
    } else if has("졸업") && has("증명") {
        "graduation"
    } else if has("성적") && has("증명") {
        "transcript"
    } else {
        return None;
    };
    by_id(id)
}

/// Words that name "documents" in general, not one document.
const GENERIC: &[&str] = &[
    "서류",
    "구비서류",
    "제출서류",
    "증빙서류",
    "필요서류",
    "신청서류",
];

pub fn is_generic(name: &str) -> bool {
    let n: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    GENERIC.contains(&n.as_str())
}

fn row(name: &str) -> Value {
    match lookup(name) {
        Some(issuer) => json!({
            "name": name,
            "known": true,
            "document": issuer.document,
            "issuer": issuer.issuer,
            "url": issuer.url,
            "guidance": issuer.guidance,
            "verified_on": issuer.verified_on,
            "login_required": issuer.url.is_some(),
        }),
        None => json!({
            "name": name,
            "known": false,
            "guidance": "발급처를 안내할 수 없어요. 공고문에서 발급 기관을 확인해 주세요.",
        }),
    }
}

/// Issue links for a named document, or every document task of the
/// programme's checklist (at most 8).
pub async fn issue_links(
    pool: &PgPool,
    user: Uuid,
    program: Option<Uuid>,
    document: Option<&str>,
) -> Result<Outcome, ToolError> {
    let mut title: Option<String> = None;
    let mut official_url: Option<String> = None;
    let mut names: Vec<String> = document
        .map(|d| vec![d.trim().to_owned()])
        .unwrap_or_default();
    if let Some(program) = program {
        let mut connection = pool.acquire().await.map_err(|error| {
            tracing::warn!(error = %error, "document link query failed");
            tool_error(
                "tool_failed",
                "정보를 불러오지 못했어요. 잠시 후 다시 시도해 주세요.",
            )
        })?;
        let plan = application_preparation::load_on(&mut connection, user, program)
            .await
            .map_err(|_| {
                tool_error(
                    "program_unavailable",
                    "지금 안내할 수 있는 혜택이 아니에요.",
                )
            })?;
        let url: Option<String> =
            sqlx::query_scalar("SELECT official_url FROM programs WHERE id=$1")
                .bind(program)
                .fetch_optional(&mut *connection)
                .await
                .ok()
                .flatten();
        // The link resolver's checked link, as the programme's own button
        // (`application_destination::shown_url`); unread: no link.
        official_url =
            match crate::services::application_destination::load(&mut connection, program).await {
                Ok(state) => {
                    crate::services::application_destination::shown_url(url.as_deref(), &state)
                }
                Err(error) => {
                    tracing::warn!(error = %error, "document link selection query failed");
                    None
                }
            };
        title = Some(plan.title.clone());
        if names.is_empty() {
            names = plan
                .tasks
                .iter()
                .filter(|t| t.kind == "document")
                .map(|t| t.title.clone())
                .take(8)
                .collect();
        }
    }
    let documents: Vec<Value> = names.iter().map(|n| row(n)).collect();
    let known = documents.iter().filter(|d| d["known"] == true).count();
    Ok(Outcome {
        summary: json!({"documents": documents.len(), "known": known}),
        data: json!({
            "program_id": program,
            "title": title,
            "catalog_version": CATALOG_VERSION,
            "documents": documents,
            "official_url": official_url,
        }),
        evidence: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_map_to_the_catalog_and_lookalikes_do_not() {
        for (name, id) in [
            ("주민등록등본", "resident_register_copy"),
            ("주민등록표 등본", "resident_register_copy"),
            ("등본", "resident_register_copy"),
            ("주민등록초본", "resident_register_abstract"),
            ("가족관계증명서", "family_relation"),
            ("가족관계 증명서(상세)", "family_relation"),
            ("기본증명서", "basic_certificate"),
            ("건강보험자격득실확인서", "health_insurance_eligibility"),
            ("건강보험 자격득실 확인서", "health_insurance_eligibility"),
            ("건강보험료 납부확인서", "health_insurance_payment"),
            ("소득금액증명원", "income_amount"),
            ("재학증명서", "enrollment"),
            ("졸업증명서", "graduation"),
            ("휴학증명서", "leave"),
            ("사업자등록증명", "business_registration"),
        ] {
            assert_eq!(lookup(name).map(|i| i.id), Some(id), "{name}");
        }
        for name in [
            "부동산 등기부등본",
            "법인등기부등본",
            "건축물대장 등본",
            "통장사본",
            "자기소개서",
            "서류",
        ] {
            assert_eq!(lookup(name), None, "{name}");
        }
        assert!(is_generic("제출 서류") && is_generic("서류") && !is_generic("등본"));
    }

    #[test]
    fn catalog_links_are_official_https_with_a_verification_date() {
        let version_date = CATALOG_VERSION.trim_start_matches("document-issuers-");
        for issuer in CATALOG {
            match issuer.url {
                Some(url) => {
                    let parsed = reqwest::Url::parse(url).unwrap();
                    assert_eq!(parsed.scheme(), "https", "{}", issuer.id);
                    assert!(
                        ISSUER_HOSTS.contains(&parsed.host_str().unwrap()),
                        "{}",
                        issuer.id
                    );
                    let verified = issuer.verified_on.expect("a link has a verification date");
                    assert!(chrono::NaiveDate::parse_from_str(verified, "%Y-%m-%d").is_ok());
                    assert!(verified <= version_date, "{}", issuer.id);
                }
                None => assert!(issuer.guidance.contains("학교"), "{}", issuer.id),
            }
        }
        let unknown = row("임대차계약서");
        assert_eq!(unknown["known"], false);
        assert!(unknown["guidance"]
            .as_str()
            .unwrap()
            .starts_with("발급처를 안내할 수 없어요"));
        assert!(unknown.get("url").is_none());
    }

    /// Network check of every catalog link (run before bumping the version):
    /// `cargo test -p api --lib issuer_urls_still_resolve -- --ignored`.
    #[tokio::test]
    #[ignore = "network"]
    async fn issuer_urls_still_resolve() {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (mazimi issuer link check)")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();
        for issuer in CATALOG {
            if let Some(url) = issuer.url {
                let status = client.get(url).send().await.map(|r| r.status().as_u16());
                assert_eq!(status.ok(), Some(200), "{url}");
            }
        }
    }

    /// Review LOW: 자격확인서 is not 자격득실확인서.
    #[test]
    fn eligibility_confirmation_is_not_the_history_certificate() {
        assert_eq!(
            lookup("건강보험 자격확인서").map(|i| i.id),
            Some("health_insurance_status")
        );
        assert_eq!(
            lookup("건강보험자격득실확인서").map(|i| i.id),
            Some("health_insurance_eligibility")
        );
        assert_ne!(
            lookup("건강보험 자격확인서").map(|i| i.guidance),
            lookup("건강보험 자격득실 확인서").map(|i| i.guidance)
        );
        assert_eq!(lookup("건강보험 자격 서류"), None, "neither one: unknown");
    }
}
