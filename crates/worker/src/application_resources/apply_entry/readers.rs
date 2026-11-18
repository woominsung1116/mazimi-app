//! Per-host readers: where does the official page's own 신청 button point?
//!
//! Each answer comes from the page's own structure, learnt from saved
//! production pages (`crates/worker/tests/fixtures/apply_links`):
//!
//! - 정부24 (`plus.gov.kr` benefit detail): the page body is rendered in
//!   the browser from `/api/...`, which robots.txt disallows; the server
//!   HTML carries no service data. The portal publishes the same service's
//!   online application site in the 보조금24 `serviceDetail` record
//!   (`온라인신청사이트URL`), which ingestion already stores; that field is
//!   the reader's input.
//! - 복지로 (`bokjiro.go.kr` welfare detail): the page embeds its service
//!   record; `onapPsbltYn` says whether online application is offered and
//!   `onapUrl` is the "온라인 신청하기" target, relative to the `/ssis-tbu`
//!   context root.
//! - 온통청년 (`youthcenter.go.kr` policy detail): the "신청바로가기" button
//!   (`.gourl`) opens `plcyList.aplyUrlAddr`, prefixing `https://` when the
//!   value has no scheme; the 온통청년 Open API returns the same field.
//! - Anything else: [`super::structure::apply_elements`] on the page.
use super::structure::{self, Found};
use mazimi_core::application_resources::public_url;
use regex::Regex;
use reqwest::Url;
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Gov24,
    Bokjiro,
    YouthCenter,
    Generic,
}

impl Reader {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gov24 => "gov24",
            Self::Bokjiro => "bokjiro",
            Self::YouthCenter => "youthcenter",
            Self::Generic => "generic",
        }
    }
}

/// What a reader needs of a programme.
#[derive(Debug, Clone, Default)]
pub struct Inputs<'a> {
    pub source_type: &'a str,
    pub official_url: Option<&'a str>,
    pub raw: Option<&'a Value>,
    /// 보조금24 `serviceDetail` fields (gov_benefits only).
    pub detail: Option<&'a Value>,
}

fn host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

fn text<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The reader for a programme: its source's portal page, else its host.
pub fn reader_for(inputs: &Inputs<'_>) -> Reader {
    let official = inputs.official_url.map(host).unwrap_or_default();
    // The source's own button field first: a 온통청년 policy whose official
    // URL is a 정부24 page still opens its `aplyUrlAddr`.
    if inputs.source_type == "youth_center" && text(inputs.raw, "aplyUrlAddr").is_some() {
        Reader::YouthCenter
    } else if inputs.source_type == "gov_benefits"
        || matches!(official.as_str(), "plus.gov.kr" | "www.gov.kr")
    {
        Reader::Gov24
    } else if official == "bokjiro.go.kr" || official.ends_with(".bokjiro.go.kr") {
        Reader::Bokjiro
    } else {
        Reader::Generic
    }
}

/// The page whose button a field-based reader stands for (evidence).
pub fn youthcenter_page(raw: Option<&Value>) -> Option<String> {
    let plcy = text(raw, "plcyNo").filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric()))?;
    Some(format!(
        "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/{plcy}"
    ))
}

/// A field value as the page's button would open it: https only (an http
/// target is not offered; the client opens HTTPS only), session paths
/// dropped, the link resolver's repairs applied.
fn as_link(value: &str, add_scheme: bool) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let value = if add_scheme && !lower.starts_with("http") {
        format!("https://{value}")
    } else if lower.starts_with("http://") {
        // The record names the site over http; the client opens HTTPS only.
        // The https form is offered only if verification can fetch it.
        format!("https://{}", &value["http://".len()..])
    } else {
        value.to_string()
    };
    crate::link_resolver::candidates::normalize(&value)
}

/// One reader's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// The button's target.
    Link {
        url: String,
        label: String,
        /// The page the button is on (or stands for).
        source_url: String,
        /// The structure that named it (`gov24_detail`, `bokjiro_onap`,
        /// `youthcenter_aply`, or the generic reader's reasons).
        why: String,
    },
    /// Several elements tie for the page's own button (generic reader).
    Tied {
        source_url: String,
        found: Vec<Found>,
    },
    /// The page offers no button: the reason code.
    None(&'static str),
}

/// 정부24: the service's `온라인신청사이트URL`.
pub fn gov24(inputs: &Inputs<'_>) -> Entry {
    let source_url = inputs.official_url.unwrap_or_default().to_string();
    match text(inputs.detail, "온라인신청사이트URL") {
        None if inputs.detail.is_none() => Entry::None("gov24_detail_missing"),
        None => Entry::None("gov24_no_online_site"),
        Some(value) => match as_link(value, false) {
            Some(url) => Entry::Link {
                url,
                label: "온라인 신청".into(),
                source_url,
                why: "gov24_detail".into(),
            },
            None => Entry::None("gov24_online_site_not_https"),
        },
    }
}

/// 온통청년: the policy's `aplyUrlAddr`.
pub fn youthcenter(inputs: &Inputs<'_>) -> Entry {
    let Some(value) = text(inputs.raw, "aplyUrlAddr") else {
        return Entry::None("youthcenter_no_apply_url");
    };
    let source_url = youthcenter_page(inputs.raw)
        .or(inputs.official_url.map(str::to_owned))
        .unwrap_or_default();
    match as_link(value, true) {
        Some(url) => Entry::Link {
            url,
            label: "신청바로가기".into(),
            source_url,
            why: "youthcenter_aply".into(),
        },
        None => Entry::None("youthcenter_apply_url_not_https"),
    }
}

static ONAP_FLAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\\*"onapPsbltYn\\*"\s*:\s*\\*"([YN])\\*""#).expect("valid"));
static ONAP_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\\*"onapUrl\\*"\s*:\s*(?:null|\\*"((?:[^"\\]|\\+[^"\\])*)\\*")"#).expect("valid")
});
static UNICODE_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\+u([0-9a-fA-F]{4})").expect("valid"));

/// A string value embedded in a JSON string inside a script: `\\u003d` and
/// `=` become `=`, `\/` becomes `/`, extra backslashes go.
fn unescape_embedded(value: &str) -> String {
    let decoded = UNICODE_ESCAPE.replace_all(value, |c: &regex::Captures<'_>| {
        u32::from_str_radix(&c[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });
    decoded.replace("\\/", "/").replace('\\', "")
}

/// 복지로: the embedded `onapPsbltYn`/`onapUrl` of the detail page.
pub fn bokjiro(page_url: &str, html: &str) -> Entry {
    let Some(flag) = ONAP_FLAG.captures(html) else {
        return Entry::None("bokjiro_fields_missing");
    };
    if &flag[1] != "Y" {
        return Entry::None("bokjiro_no_online_application");
    }
    let Some(value) = ONAP_URL
        .captures(html)
        .and_then(|c| c.get(1).map(|m| unescape_embedded(m.as_str())))
        .filter(|v| !v.trim().is_empty())
    else {
        return Entry::None("bokjiro_online_url_missing");
    };
    let Ok(page) = Url::parse(page_url) else {
        return Entry::None("bokjiro_page_url_invalid");
    };
    // `onapUrl` is relative to the application's context root (`/ssis-tbu`).
    let root = page
        .path_segments()
        .and_then(|mut s| s.next())
        .filter(|s| !s.is_empty())
        .map(|s| format!("/{s}"))
        .unwrap_or_default();
    let value = value.trim();
    let joined =
        if value.starts_with('/') && !root.is_empty() && !value.starts_with(&format!("{root}/")) {
            page.join(&format!("{root}{value}"))
        } else {
            page.join(value)
        };
    match joined.ok().map(|u| u.to_string()).filter(|u| public_url(u)) {
        Some(url) => Entry::Link {
            url,
            label: "온라인 신청하기".into(),
            source_url: page_url.to_string(),
            why: "bokjiro_onap".into(),
        },
        None => Entry::None("bokjiro_online_url_invalid"),
    }
}

/// Any page: its own application element (see [`structure`]).
pub fn generic(page_url: &str, html: &str) -> Entry {
    let found = structure::apply_elements(page_url, html);
    let best = structure::best(&found);
    match best.as_slice() {
        [] => Entry::None("no_apply_element"),
        [one] => Entry::Link {
            url: one.url.clone(),
            label: one.label.clone(),
            source_url: page_url.to_string(),
            why: one.why.clone(),
        },
        tied => Entry::Tied {
            source_url: page_url.to_string(),
            found: tied.iter().map(|f| (*f).clone()).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn readers_follow_the_source_portal_then_the_host() {
        let raw = json!({"aplyUrlAddr": "https://www.ch2030youth.kr"});
        let r = |source: &str, official: &str, raw: Option<&Value>| {
            reader_for(&Inputs {
                source_type: source,
                official_url: Some(official),
                raw,
                detail: None,
            })
        };
        assert_eq!(
            r(
                "gov_benefits",
                "https://plus.gov.kr/portal/benefitV2/x",
                None
            ),
            Reader::Gov24
        );
        assert_eq!(
            r(
                "youth_center",
                "https://youth.chungnam.go.kr/web/view?bizId=1",
                Some(&raw)
            ),
            Reader::YouthCenter
        );
        assert_eq!(
            r(
                "youth_center",
                "https://youth.chungnam.go.kr/web/view?bizId=1",
                Some(&json!({"aplyUrlAddr": " "}))
            ),
            Reader::Generic
        );
        assert_eq!(r("local_welfare", "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=W1", None), Reader::Bokjiro);
        assert_eq!(
            r(
                "youth_center",
                "https://www.bokjiro.go.kr.evil.example/x",
                None
            ),
            Reader::Generic
        );
        // The policy's own button field wins over a 정부24 official URL.
        assert_eq!(
            r("youth_center", "https://www.gov.kr/portal/x", Some(&raw)),
            Reader::YouthCenter
        );
    }

    #[test]
    fn field_readers_open_what_the_button_opens() {
        let detail = json!({"온라인신청사이트URL": "https://www.work24.go.kr/cm/apply.do?x=1"});
        let gov = gov24(&Inputs {
            source_type: "gov_benefits",
            official_url: Some("https://plus.gov.kr/portal/benefitV2/d?svcId=1"),
            raw: None,
            detail: Some(&detail),
        });
        assert!(
            matches!(&gov, Entry::Link { url, .. } if url == "https://www.work24.go.kr/cm/apply.do?x=1")
        );
        assert_eq!(
            gov24(&Inputs {
                detail: Some(&json!({})),
                ..Inputs::default()
            }),
            Entry::None("gov24_no_online_site")
        );
        assert_eq!(
            gov24(&Inputs::default()),
            Entry::None("gov24_detail_missing")
        );
        // An http value is tried over https; verification decides.
        assert!(matches!(
            gov24(&Inputs {
                detail: Some(&json!({"온라인신청사이트URL": "http://www.x.go.kr/a"})),
                ..Inputs::default()
            }),
            Entry::Link { url, .. } if url == "https://www.x.go.kr/a"
        ));
        assert_eq!(
            gov24(&Inputs {
                detail: Some(&json!({"온라인신청사이트URL": "ftp://www.x.go.kr/a"})),
                ..Inputs::default()
            }),
            Entry::None("gov24_online_site_not_https")
        );
        // 온통청년 adds https:// to a bare host, as its own button does.
        let raw =
            json!({"aplyUrlAddr": "apply.example.go.kr/youth/1", "plcyNo": "20260918005400113439"});
        match youthcenter(&Inputs {
            source_type: "youth_center",
            raw: Some(&raw),
            ..Inputs::default()
        }) {
            Entry::Link {
                url, source_url, ..
            } => {
                assert_eq!(url, "https://apply.example.go.kr/youth/1");
                assert!(source_url.ends_with("/ythPlcyDetail/20260918005400113439"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn embedded_escapes_are_decoded() {
        assert_eq!(
            unescape_embedded(
                r"/twoaa/aplySrvChc/selectCvlcptServChs.do?menuGb\\u003dtwoaa\\u0026srvGb\\u003d020"
            ),
            "/twoaa/aplySrvChc/selectCvlcptServChs.do?menuGb=twoaa&srvGb=020"
        );
        assert_eq!(unescape_embedded(r"\/a\/b=1"), "/a/b=1");
    }
}
