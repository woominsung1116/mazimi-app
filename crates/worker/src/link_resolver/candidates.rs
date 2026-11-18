//! Link candidates of one programme, in the source's preference order
//! (design 5.3), with the origin each one may vouch for (design 5.4).
use mazimi_core::{
    application_resources::public_url,
    source_policy::{
        has_punycode_label, is_shared_hosting, official_site_covers, registrable_domain,
    },
};
use regex::Regex;
use reqwest::Url;
use serde_json::Value;
use std::sync::LazyLock;

/// A field the link was read from. `field_rank` orders fields of one
/// source; lower wins between links of the same kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub field: &'static str,
    pub field_rank: i16,
    pub url: String,
    pub origin: Origin,
    /// Read from free text (a method or deadline sentence), not a URL field.
    pub free_text: bool,
}

/// Why a host may be shown for this programme. Only `PublicSector`,
/// `Allowlist` and `FormHost` are vetted for an application label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The source named this URL in one of the programme's URL fields, or
    /// free text points inside such a host's site.
    Agency,
    /// A government, university or research-institute domain.
    PublicSector,
    /// An approved source domain or a listed public institution.
    Allowlist,
    /// A form service, read from the field that names the application form.
    FormHost,
    /// None of the above: never shown.
    Rejected,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agency => "agency",
            Self::PublicSector => "public_sector",
            Self::Allowlist => "allowlist",
            Self::FormHost => "form_host",
            Self::Rejected => "rejected",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "agency" => Self::Agency,
            "public_sector" => Self::PublicSector,
            "allowlist" => Self::Allowlist,
            "form_host" => Self::FormHost,
            _ => Self::Rejected,
        }
    }
}

/// Field the resolver adds itself: an application link found on the
/// selected notice page (design 5.8).
pub const ONE_HOP_FIELD: &str = "notice_apply_link";
pub const ONE_HOP_RANK: i16 = 90;

/// Fields that name the application form; only these may point at a form
/// service (Google Forms, 네이버 폼 ...).
const FORM_FIELDS: &[&str] = &["aplyUrlAddr", "온라인신청사이트URL"];
const FORM_SERVICES: &[&str] = &[
    "docs.google.com",
    "forms.gle",
    "form.naver.com",
    "naver.me",
    "forms.office.com",
    "tally.so",
    "typeform.com",
    "smore.im",
];
const PUBLIC_SECTOR_SUFFIXES: &[&str] = &[".go.kr", ".ac.kr", ".re.kr", ".mil.kr"];

static TEXT_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?://|www\.)[^\s"'<>()\[\]가-힣,]+"#).expect("valid")
});
static BARE_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^www\.|^[a-z0-9-]+(\.[a-z0-9-]+)+(/|$)").expect("valid"));
/// A servlet session in the path (`;jsessionid=...`): the checker's session,
/// never a user's.
static SESSION_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i);jsessionid=[^?#/]*").expect("valid"));
/// `.../tuition.dopg=x&y=z`: the `?` before the first parameter is missing.
static MISSING_QUERY_MARK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(https://[^/?#]+/[^?#]*?\.(?:do|jsp|php|aspx?|html?))([A-Za-z_][A-Za-z0-9_]*=[^?#]*)$")
        .expect("valid")
});

fn is_form_service(host: &str) -> bool {
    FORM_SERVICES
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

pub(crate) fn is_public_sector(host: &str) -> bool {
    PUBLIC_SECTOR_SUFFIXES.iter().any(|s| host.ends_with(s))
}

/// The audit's `norm`, then the repair rules, then the navigation rule:
/// `None` unless the result is a public, credential-free HTTPS URL.
pub fn normalize(raw: &str) -> Option<String> {
    let mut value = raw
        .trim()
        .trim_end_matches(['.', ',', ';', ')'])
        .to_string();
    if value.is_empty() {
        return None;
    }
    if BARE_HOST.is_match(&value) && !value.contains("://") {
        value = format!("http://{value}");
    }
    if !value.to_ascii_lowercase().starts_with("https://") {
        // http:// links are not offered: the client opens HTTPS only.
        return None;
    }
    value.replace_range(..8, "https://");
    value = SESSION_PATH.replace_all(&value, "").into_owned();
    if let Some(found) = MISSING_QUERY_MARK.captures(&value) {
        value = format!("{}?{}", &found[1], &found[2]);
    }
    if is_admin_console(&value) || !public_url(&value) {
        return None;
    }
    Some(value)
}

/// Back-office pages a registrar pasted instead of the public page.
fn is_admin_console(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        let host = u.host_str().unwrap_or("").trim_start_matches("www.");
        host == "youthcenter.go.kr" && u.path().contains("manageYthPlcy")
    })
}

fn text_urls(text: &str) -> Vec<&str> {
    TEXT_URL.find_iter(text).map(|m| m.as_str()).collect()
}

fn text_of<'a>(raw: &'a Value, key: &str) -> Option<&'a str> {
    raw.get(key).and_then(Value::as_str)
}

/// URLs taken from one free-text field, and candidates per programme.
const MAX_TEXT_URLS: usize = 5;
const MAX_CANDIDATES: usize = 32;
/// Ranks of one field: `field index * FIELD_RANK_STEP + position`, so a
/// field missing from one read never shifts the others' ranks.
pub const FIELD_RANK_STEP: i16 = 10;

struct Builder {
    found: Vec<(&'static str, String, bool, i16)>,
    fields: i16,
}

impl Builder {
    fn next_base(&mut self) -> i16 {
        self.fields += 1;
        (self.fields - 1) * FIELD_RANK_STEP
    }
    fn field(&mut self, field: &'static str, value: Option<&str>) {
        let base = self.next_base();
        if let Some(url) = value.and_then(normalize) {
            self.found.push((field, url, false, base));
        }
    }
    /// Keeps the rank slot of a field that is not in this read.
    fn skip(&mut self) {
        self.next_base();
    }
    fn text(&mut self, field: &'static str, value: Option<&str>) {
        let base = self.next_base();
        let urls = value.map(text_urls).unwrap_or_default();
        let found = urls.into_iter().filter_map(normalize).take(MAX_TEXT_URLS);
        for (k, url) in found.enumerate() {
            self.found.push((field, url, true, base + k as i16));
        }
    }
}

/// Output of [`extract`].
#[derive(Debug, Default)]
pub struct Extracted {
    pub candidates: Vec<Candidate>,
    /// Fields whose input was missing from this read (a failed detail call):
    /// their stored candidates are kept, not removed.
    pub unknown_fields: Vec<&'static str>,
}

/// Candidates of one programme. `detail` is the gov24 `serviceDetail` row
/// (`None` when it was not read); `approved` lists approved source domains.
pub fn extract(
    source_type: &str,
    official_url: Option<&str>,
    raw: &Value,
    detail: Option<&Value>,
    approved: &[String],
) -> Extracted {
    let mut b = Builder {
        found: Vec::new(),
        fields: 0,
    };
    let mut unknown_fields = Vec::new();
    match source_type {
        "youth_center" => {
            b.field("aplyUrlAddr", text_of(raw, "aplyUrlAddr"));
            b.field("refUrlAddr1", text_of(raw, "refUrlAddr1"));
            b.field("refUrlAddr2", text_of(raw, "refUrlAddr2"));
            b.text("plcyAplyMthdCn_text", text_of(raw, "plcyAplyMthdCn"));
            b.text("etcMttrCn_text", text_of(raw, "etcMttrCn"));
            let plcy =
                text_of(raw, "plcyNo").filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric()));
            b.field(
                "youthcenter_detail",
                plcy.map(|n| {
                    format!("https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/{n}")
                })
                .as_deref(),
            );
        }
        "gov_benefits" => {
            match detail {
                Some(row) => b.field("온라인신청사이트URL", text_of(row, "온라인신청사이트URL")),
                None => {
                    b.skip();
                    unknown_fields.push("온라인신청사이트URL")
                }
            }
            b.text("접수기관_text", text_of(raw, "접수기관"));
            b.text("신청기한_text", text_of(raw, "신청기한"));
            b.field("상세조회URL", text_of(raw, "상세조회URL"));
        }
        "local_welfare" | "national_welfare" => b.field("servDtlLink", text_of(raw, "servDtlLink")),
        "scholarship" => b.field("홈페이지 주소", text_of(raw, "홈페이지 주소")),
        _ => {}
    }
    b.field("official_url", official_url);

    // Hosts the source itself named for this programme.
    let agency: Vec<String> = b
        .found
        .iter()
        .filter(|(_, _, free, _)| !free)
        .map(|(_, url, _, _)| super::classify::host_of(url))
        .filter(|h| !is_shared_hosting(h) && !is_form_service(h))
        .collect();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (field, url, free_text, rank) in b.found {
        if candidates.iter().any(|c| c.url == url) {
            continue;
        }
        if candidates.len() == MAX_CANDIDATES {
            break;
        }
        let host = super::classify::host_of(&url);
        let origin = origin_of(&host, field, free_text, &agency, approved);
        candidates.push(Candidate {
            field,
            field_rank: rank,
            url,
            origin,
            free_text,
        });
    }
    Extracted {
        candidates,
        unknown_fields,
    }
}

/// Design 5.4 origin rule.
pub fn origin_of(
    host: &str,
    field: &str,
    free_text: bool,
    agency: &[String],
    approved: &[String],
) -> Origin {
    if is_form_service(host) {
        return if FORM_FIELDS.contains(&field) {
            Origin::FormHost
        } else {
            Origin::Rejected
        };
    }
    let covered = |domain: &String| official_site_covers(domain, host);
    if is_public_sector(host) && !has_punycode_label(host) {
        return Origin::PublicSector;
    }
    let listed = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    // Public institutions on commercial or organisation domains that notices
    // send applicants to: one table in `mazimi_core`.
    if approved.iter().any(|d| listed(d))
        || mazimi_core::application_resources::PUBLIC_INSTITUTION_HOSTS
            .iter()
            .any(|d| listed(d))
    {
        return Origin::Allowlist;
    }
    // A URL field is the source's own statement, a city's blog post
    // included; an unvetted host is shown at most as a notice
    // (`resolve::shown`).
    if !free_text || agency.iter().any(covered) {
        return Origin::Agency;
    }
    Origin::Rejected
}

/// An application link found on a notice page: same site as the page, a
/// public HTTPS URL, not the page itself and not a homepage.
pub fn one_hop_target(page_url: &str, href: &str) -> Option<String> {
    // An unrendered template (`${item.id}`, `{{id}}`) is not a link.
    if href.contains("${") || href.contains("{{") || href.to_ascii_lowercase().contains("%7b") {
        return None;
    }
    let base = Url::parse(page_url).ok()?;
    let target = base.join(href.trim()).ok()?;
    // A link on a fetched page can carry the checker's own session or CSRF
    // token; such a URL is never shown to anyone.
    let session_key = target.query_pairs().any(|(k, _)| {
        let k = k.to_ascii_lowercase();
        ["token", "csrf", "sess", "ticket", "auth"]
            .iter()
            .any(|w| k.contains(w))
            || k.starts_with("saml")
            || matches!(k.as_str(), "sid" | "nonce" | "state" | "code")
    });
    if session_key || target.path().contains("(S(") || target.path().contains("(A(") {
        return None;
    }
    let value = target.to_string();
    let (page_host, host) = (base.host_str()?, target.host_str().unwrap_or(""));
    // An IDN host vouches only for itself (homographs).
    let same_site = if has_punycode_label(page_host) || has_punycode_label(host) {
        page_host == host
    } else {
        registrable_domain(page_host)
            .is_some_and(|d| registrable_domain(host).as_deref() == Some(&d))
    };
    (same_site
        && target.as_str() != base.as_str()
        && !is_shared_hosting(target.host_str().unwrap_or(""))
        && !super::classify::is_generic(&value))
    .then(|| normalize(&value))
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn urls(e: &Extracted) -> Vec<(&str, &str, Origin)> {
        e.candidates
            .iter()
            .map(|c| (c.field, c.url.as_str(), c.origin))
            .collect()
    }

    #[test]
    fn youth_center_keeps_every_field_in_preference_order() {
        let raw = json!({
            "aplyUrlAddr": "", "refUrlAddr1": "https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1",
            "refUrlAddr2": "www.wonju.go.kr", "plcyNo": "20261019005400214010",
            "plcyAplyMthdCn": "온라인 접수(https://apply.wonju.go.kr/youth/aply.do?id=3).",
        });
        let e = extract(
            "youth_center",
            Some("https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1"),
            &raw,
            None,
            &[],
        );
        assert_eq!(
            urls(&e),
            vec![
                ("refUrlAddr1", "https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1", Origin::PublicSector),
                ("plcyAplyMthdCn_text", "https://apply.wonju.go.kr/youth/aply.do?id=3", Origin::PublicSector),
                ("youthcenter_detail", "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/20261019005400214010", Origin::PublicSector),
            ]
        );
        assert!(e.candidates[1].free_text);
        assert!(e.candidates[0].field_rank < e.candidates[1].field_rank);
    }

    #[test]
    fn gov24_detail_comes_first_and_a_missing_detail_is_unknown_not_empty() {
        let raw = json!({"상세조회URL": "https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1", "접수기관": "주민센터"});
        let detail = json!({"온라인신청사이트URL": "https://www.bokjiro.go.kr/ssis-tbu/apply/1"});
        let e = extract("gov_benefits", None, &raw, Some(&detail), &[]);
        assert_eq!(e.candidates[0].field, "온라인신청사이트URL");
        assert_eq!(e.candidates[1].field, "상세조회URL");
        let e = extract("gov_benefits", None, &raw, None, &[]);
        assert_eq!(e.unknown_fields, vec!["온라인신청사이트URL"]);
        assert_eq!(e.candidates.len(), 1);
    }

    /// The audit's malformed kosaf URLs and the 온통청년 admin console, by rule.
    #[test]
    fn repair_and_rejection_rules() {
        assert_eq!(
            normalize(
                "https://www.kosaf.go.kr/ko/tuition.dopg=tuition04_11_01&naviParam=HD01020601"
            )
            .as_deref(),
            Some("https://www.kosaf.go.kr/ko/tuition.do?pg=tuition04_11_01&naviParam=HD01020601")
        );
        assert_eq!(
            normalize("https://www.kosaf.go.kr/ko/tuition.do?pg=tuition19").as_deref(),
            Some("https://www.kosaf.go.kr/ko/tuition.do?pg=tuition19")
        );
        assert_eq!(
            normalize("https://www.example.go.kr/a/view.do"),
            Some("https://www.example.go.kr/a/view.do".into())
        );
        assert!(normalize("https://www.youthcenter.go.kr/manageYthPlcy/ythPlcyManageMain/ythPlcyManageRegistry/ypmRegistryStep01").is_none());
        for bad in [
            "http://www.gijang.go.kr/",
            "www.kosaf.go.kr",
            "javascript:alert(1)",
            "https://u:p@x.go.kr/a",
            "https://127.0.0.1/a",
            "",
        ] {
            assert!(normalize(bad).is_none(), "{bad}");
        }
        assert_eq!(
            normalize(" https://www.x.go.kr/apply). ").as_deref(),
            Some("https://www.x.go.kr/apply")
        );
        assert_eq!(
            normalize(
                "https://www.atfis.or.kr/home/intern/application.do;jsessionid=FC63A0953?act=agree"
            )
            .as_deref(),
            Some("https://www.atfis.or.kr/home/intern/application.do?act=agree")
        );
    }

    #[test]
    fn origin_rule_limits_form_services_and_free_text() {
        let agency = vec![
            "www.wonju.go.kr".to_string(),
            "www.youth-wonju.or.kr".to_string(),
        ];
        assert_eq!(
            origin_of("forms.gle", "aplyUrlAddr", false, &agency, &[]),
            Origin::FormHost
        );
        assert_eq!(
            origin_of("docs.google.com", "refUrlAddr1", false, &agency, &[]),
            Origin::Rejected
        );
        assert_eq!(
            origin_of("docs.google.com", "plcyAplyMthdCn_text", true, &agency, &[]),
            Origin::Rejected
        );
        assert_eq!(
            origin_of(
                "apply.youth-wonju.or.kr",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &[]
            ),
            Origin::Agency
        );
        assert_eq!(
            origin_of(
                "www.work24.go.kr",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &[]
            ),
            Origin::PublicSector
        );
        assert_eq!(
            origin_of(
                "apply.jobaba.net",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &[]
            ),
            Origin::Allowlist
        );
        assert_eq!(
            origin_of(
                "shop.example.com",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &[]
            ),
            Origin::Rejected
        );
        assert_eq!(
            origin_of("blog.naver.com", "refUrlAddr1", false, &agency, &[]),
            Origin::Agency
        );
        assert_eq!(
            origin_of("blog.naver.com", "plcyAplyMthdCn_text", true, &agency, &[]),
            Origin::Rejected
        );
        assert_eq!(
            origin_of("www.kupc.or.kr", "refUrlAddr1", false, &agency, &[]),
            Origin::Agency
        );
        assert_eq!(
            origin_of(
                "blog.naver.com",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &["blog.naver.com".into()]
            ),
            Origin::Allowlist
        );
    }

    #[test]
    fn one_hop_targets_stay_on_the_notice_site() {
        let page = "https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1";
        assert_eq!(
            one_hop_target(page, "/youth/aplyReqst.do?id=3").as_deref(),
            Some("https://www.wonju.go.kr/youth/aplyReqst.do?id=3")
        );
        assert_eq!(
            one_hop_target(page, "https://apply.wonju.go.kr/form/3").as_deref(),
            Some("https://apply.wonju.go.kr/form/3")
        );
        let idn = "https://www.xn--hc0by27bu6atul3dc6t.kr/main/rentSubsidy";
        assert!(one_hop_target(idn, "/main/rentSubsidy/service").is_some());
        assert!(one_hop_target(idn, "https://other.xn--hc0by27bu6atul3dc6t.kr/a").is_none());
        for href in [
            "https://forms.gle/x",
            "https://evil.example/apply",
            "javascript:void(0)",
            "/",
            "/main.do",
            page,
            "/pbanc/view/${item.no}",
            "/pbanc/view/$%7Bitem.no%7D",
        ] {
            assert!(one_hop_target(page, href).is_none(), "{href}");
        }
    }

    #[test]
    fn review_follow_ups_on_origins_ranks_and_session_links() {
        let agency = vec!["www.wonju.go.kr".to_string()];
        assert_eq!(
            origin_of("www.wonju.go.kr", "refUrlAddr1", false, &agency, &[]),
            Origin::PublicSector
        );
        assert_eq!(
            origin_of(
                "apply.wonju.go.kr",
                "plcyAplyMthdCn_text",
                true,
                &agency,
                &[]
            ),
            Origin::PublicSector
        );
        assert_eq!(
            origin_of("www.kupc.or.kr", "refUrlAddr1", false, &agency, &[]),
            Origin::Agency
        );
        assert_eq!(
            normalize("HTTPS://www.x.go.kr/a").as_deref(),
            Some("https://www.x.go.kr/a")
        );
        // A missing detail keeps the other fields' ranks.
        let raw = json!({"상세조회URL": "https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1"});
        let with = extract("gov_benefits", None, &raw, Some(&json!({})), &[]);
        let without = extract("gov_benefits", None, &raw, None, &[]);
        assert_eq!(
            with.candidates[0].field_rank,
            without.candidates[0].field_rank
        );
        // Free text gives at most five URLs.
        let text: String = (0..9)
            .map(|i| format!(" https://www.x.go.kr/a{i} "))
            .collect();
        let e = extract(
            "youth_center",
            None,
            &json!({"plcyAplyMthdCn": text}),
            None,
            &[],
        );
        assert_eq!(e.candidates.iter().filter(|c| c.free_text).count(), 5);
        let page = "https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1";
        for href in [
            "/apply?_token=1",
            "/apply?__RequestVerificationToken=1",
            "/apply?ticket=ST-1",
            "/(S(abc))/apply",
            "/apply?PHPSESSID=1",
        ] {
            assert!(one_hop_target(page, href).is_none(), "{href}");
        }
    }
}
