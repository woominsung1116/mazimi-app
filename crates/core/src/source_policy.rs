//! Shared URL and discovery policy for corporate-benefit sources.
//!
//! Keeping these rules in one crate prevents the admin API, search discovery,
//! and direct fetcher from accepting subtly different URL scopes.

use std::net::IpAddr;
use thiserror::Error;
use url::Url;

const TRACKING_QUERY_KEYS: &[&str] = &["ref", "referer", "source", "campaign", "t_click", "t_page"];

const BENEFIT_KEYWORDS: &[&str] = &[
    "혜택",
    "할인",
    "이벤트",
    "쿠폰",
    "멤버십",
    "제휴",
    "프로모션",
    "리워드",
    "페이백",
    "청년",
    "대학생",
    "event",
    "benefit",
    "discount",
    "coupon",
    "membership",
    "promotion",
    "reward",
];

const INTERACTIVE_PATH_MARKERS: &[&str] = &[
    "/login",
    "/logout",
    "/signin",
    "/signup",
    "/account",
    "/mypage",
    "/my/",
    "/cart",
    "/basket",
    "/checkout",
    "/order",
    "/payment",
    "/admin",
    // Marketplace product-detail routes are purchase content, not benefit
    // evidence. Keep these specific enough to avoid blocking documentation
    // paths such as `/education/products/office`.
    "/vp/products/",
    "/goods/view",
    "/product/detail",
    "/product/view",
    "/item/detail",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublicUrlError {
    #[error("invalid URL")]
    Invalid,
    #[error("only credential-free HTTPS URLs on port 443 are allowed")]
    UnsafeSchemeOrAuthority,
    #[error("public DNS hostnames only")]
    NonPublicHost,
}

#[derive(Debug, Clone)]
pub struct CanonicalPublicUrl {
    pub url: Url,
    pub domain: String,
    pub canonical_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Index,
    Detail,
    Unknown,
}

impl CandidateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Detail => "detail",
            Self::Unknown => "unknown",
        }
    }
}

pub fn canonicalize_public_https_url(value: &str) -> Result<CanonicalPublicUrl, PublicUrlError> {
    let mut url = Url::parse(value.trim()).map_err(|_| PublicUrlError::Invalid)?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(PublicUrlError::UnsafeSchemeOrAuthority);
    }
    let domain = url
        .host_str()
        .ok_or(PublicUrlError::NonPublicHost)?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if domain == "localhost"
        || domain.ends_with(".localhost")
        || !domain.contains('.')
        || domain.parse::<IpAddr>().is_ok()
    {
        return Err(PublicUrlError::NonPublicHost);
    }
    url.set_fragment(None);
    strip_tracking_query_parameters(&mut url);
    Ok(CanonicalPublicUrl {
        canonical_url: canonical_url_string(&url),
        url,
        domain,
    })
}

/// Dedup key for a fragment-free, tracking-free URL. Query pairs are sorted
/// and a trailing slash is removed from non-root paths so `?b=1&a=2` and
/// `/events/` cannot create a second candidate or program for one page. The
/// fetch URL itself is left as the site published it.
pub fn canonical_url_string(url: &Url) -> String {
    let mut canonical = url.clone();
    canonical.set_fragment(None);
    let mut pairs: Vec<(String, String)> = canonical
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    pairs.sort();
    canonical.set_query(None);
    if !pairs.is_empty() {
        let mut serializer = canonical.query_pairs_mut();
        for (key, value) in &pairs {
            serializer.append_pair(key, value);
        }
    }
    let trimmed = canonical.path().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        canonical.set_path("/");
    } else if trimmed != canonical.path() {
        canonical.set_path(&trimmed);
    }
    canonical.to_string()
}

pub fn strip_tracking_query_parameters(url: &mut Url) {
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_") && !TRACKING_QUERY_KEYS.contains(&key.as_str())
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    if !retained.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in retained {
            pairs.append_pair(&key, &value);
        }
    }
}

/// Second-level public suffixes under which the registrable name is the
/// third label (`busan.go.kr` -> `busan.go.kr`, `www.busan.go.kr` -> same).
/// A bounded list, not the full Public Suffix List: Korean institutional and
/// regional suffixes plus the common foreign ones seen in official notices.
/// Unknown hosts fall back to the last two labels. That is the stricter
/// reading for ordinary domains, but not for platforms that host many
/// unrelated owners (`github.io`, `naver.com`): see `SHARED_HOSTING_*` and
/// `official_site_covers`, which every "same institution" decision must use.
const SECOND_LEVEL_PUBLIC_SUFFIXES: &[&str] = &[
    "go.kr",
    "or.kr",
    "co.kr",
    "ac.kr",
    "re.kr",
    "ne.kr",
    "pe.kr",
    "hs.kr",
    "ms.kr",
    "es.kr",
    "sc.kr",
    "kg.kr",
    "mil.kr",
    "seoul.kr",
    "busan.kr",
    "daegu.kr",
    "incheon.kr",
    "gwangju.kr",
    "daejeon.kr",
    "ulsan.kr",
    "sejong.kr",
    "gyeonggi.kr",
    "gangwon.kr",
    "chungbuk.kr",
    "chungnam.kr",
    "jeonbuk.kr",
    "jeonnam.kr",
    "gyeongbuk.kr",
    "gyeongnam.kr",
    "jeju.kr",
    "co.jp",
    "or.jp",
    "ne.jp",
    "ac.jp",
    "go.jp",
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "com.au",
    "org.au",
    "gov.au",
    "edu.au",
    "com.cn",
    "gov.cn",
    "com.tw",
    "com.sg",
    "gov.sg",
];

/// eTLD+1 for an ASCII host, lower-cased. None for IP literals, single-label
/// hosts and hosts that are themselves a public suffix.
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return None;
    }
    let last_two = labels[labels.len() - 2..].join(".");
    let needed = if SECOND_LEVEL_PUBLIC_SUFFIXES.contains(&last_two.as_str()) {
        3
    } else {
        2
    };
    (labels.len() >= needed).then(|| labels[labels.len() - needed..].join("."))
}

/// Platforms whose subdomains belong to different owners
/// (`busan-youth.github.io` vs `attacker.github.io`). A program's official
/// URL on one of them vouches only for its own exact host. Includes the
/// Public Suffix List private-section entries seen in Korean notices.
pub const SHARED_HOSTING_SUBDOMAIN_TENANTS: &[&str] = &[
    "github.io",
    "tistory.com",
    "blogspot.com",
    "notion.site",
    "wixsite.com",
    "wordpress.com",
    "weebly.com",
    "imweb.me",
    "modoo.at",
    "cafe24.com",
    "typeform.com",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "web.app",
    "firebaseapp.com",
    "herokuapp.com",
    "azurewebsites.net",
    "cloudfront.net",
    "amazonaws.com",
];

/// Platforms where anyone can publish under the same host, separated only
/// by path (`blog.naver.com/<id>`, `docs.google.com/forms/<id>`,
/// `form.naver.com/response/<id>`). Their official URL vouches for nothing:
/// a destination there needs an exact approved source-domain entry.
pub const SHARED_HOSTING_PATH_TENANTS: &[&str] = &[
    "naver.com",
    "naver.me",
    "daum.net",
    "kakao.com",
    "google.com",
    "google.co.kr",
    "forms.gle",
    "notion.so",
    "linktr.ee",
    "instagram.com",
    "facebook.com",
    "youtube.com",
    "band.us",
    "x.com",
    "twitter.com",
];

/// The registrable domain is a multi-owner hosting platform.
pub fn is_shared_hosting(host: &str) -> bool {
    registrable_domain(host).is_some_and(|domain| {
        SHARED_HOSTING_SUBDOMAIN_TENANTS.contains(&domain.as_str())
            || SHARED_HOSTING_PATH_TENANTS.contains(&domain.as_str())
    })
}

/// Whether a program's official host vouches for `host` as the same
/// institution: same registrable domain, except on shared hosting, where
/// only the exact tenant host counts and path-tenant platforms never do.
pub fn official_site_covers(official_host: &str, host: &str) -> bool {
    let official_host = official_host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let Some(domain) = registrable_domain(&official_host) else {
        return false;
    };
    if registrable_domain(&host).as_deref() != Some(domain.as_str()) {
        return false;
    }
    if SHARED_HOSTING_PATH_TENANTS.contains(&domain.as_str()) {
        return false;
    }
    if SHARED_HOSTING_SUBDOMAIN_TENANTS.contains(&domain.as_str()) {
        return host == official_host && host != domain;
    }
    true
}

/// IDN hosts arrive from the `url` crate already punycode-encoded. Any
/// `xn--` label can be a homograph of an institution name and is never
/// treated as "within the official domain" by suffix alone.
pub fn has_punycode_label(host: &str) -> bool {
    host.split('.')
        .any(|label| label.to_ascii_lowercase().starts_with("xn--"))
}

pub fn host_is_within(host: &str, approved_domain: &str, allow_subdomains: bool) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let approved_domain = approved_domain.trim_end_matches('.').to_ascii_lowercase();
    host == approved_domain || (allow_subdomains && host.ends_with(&format!(".{approved_domain}")))
}

pub fn path_is_allowed(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        prefix == "/"
            || path == prefix
            || (path.starts_with(prefix)
                && (prefix.ends_with('/') || path.as_bytes().get(prefix.len()) == Some(&b'/')))
    })
}

pub fn classify_candidate_kind(url: &Url) -> CandidateKind {
    let path = url.path().to_ascii_lowercase();
    let has_detail_key = url.query_pairs().any(|(key, _)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "id" | "no" | "seq" | "evtno" | "evntno" | "eventid"
        )
    });
    if has_detail_key || path.contains("detail") || path.contains("view") {
        CandidateKind::Detail
    } else if path.contains("list")
        || path.ends_with("/event")
        || path.ends_with("/events")
        || path.ends_with("/event/")
        || path.ends_with("/events/")
    {
        CandidateKind::Index
    } else {
        CandidateKind::Unknown
    }
}

pub fn contains_benefit_signal(value: &str, extra_terms: &[String]) -> bool {
    let value = value.to_ascii_lowercase();
    BENEFIT_KEYWORDS
        .iter()
        .any(|keyword| value.contains(keyword))
        || extra_terms
            .iter()
            .map(|term| term.trim().to_ascii_lowercase())
            .filter(|term| !term.is_empty())
            .any(|term| value.contains(&term))
}

pub fn is_interactive_or_purchase_url(url: &Url) -> bool {
    let value = format!("{}?{}", url.path(), url.query().unwrap_or("")).to_ascii_lowercase();
    INTERACTIVE_PATH_MARKERS
        .iter()
        .any(|marker| value.contains(marker))
}

/// What `/robots.txt` answered (after redirects), before any policy.
/// Crawlers cache this fact and apply their caller's [`RobotsPolicy`] when
/// they read it, so a strict and an RFC 9309 caller can share one cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RobotsAnswer {
    /// 200 with a robots.txt body.
    Rules(String),
    /// 200 with an HTML page: a bot-challenge interstitial, or the error
    /// page a redirect landed on.
    Html,
    /// A 4xx status (401, 403, 404, ...).
    ClientError(u16),
    /// No usable answer: 429, 5xx, a network failure, a redirect off the
    /// public web. `code` is recorded.
    Unavailable(String),
}

/// How a caller reads a [`RobotsAnswer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RobotsPolicy {
    /// The default: an HTML answer is a bot wall (the corporate crawler's
    /// `robots_html`), 401/403 disallow everything, any other 4xx means no
    /// robots.txt, and no answer fails closed.
    #[default]
    Strict,
    /// RFC 9309 2.3.1.3-2.3.1.4, for callers the owner has approved it for
    /// (the apply-link reader on government hosts, the forms collector):
    /// an HTML answer or any 4xx means robots.txt is unavailable, so no
    /// restrictions; 429, 5xx and network failures still fail closed.
    Rfc9309,
}

/// A policy's reading of a robots answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RobotsVerdict {
    /// No restrictions.
    Allow,
    /// Follow these rules.
    Rules(String),
    /// Every path is disallowed.
    DisallowAll,
    /// Treat the origin as not answering (disallowed for now), with a code.
    Unavailable(String),
}

impl RobotsPolicy {
    pub fn verdict(self, answer: &RobotsAnswer) -> RobotsVerdict {
        match (self, answer) {
            (_, RobotsAnswer::Rules(body)) => RobotsVerdict::Rules(body.clone()),
            (_, RobotsAnswer::Unavailable(code)) => RobotsVerdict::Unavailable(code.clone()),
            (Self::Strict, RobotsAnswer::Html) => RobotsVerdict::Unavailable("robots_html".into()),
            (Self::Strict, RobotsAnswer::ClientError(401 | 403)) => RobotsVerdict::DisallowAll,
            (Self::Strict, RobotsAnswer::ClientError(_)) => RobotsVerdict::Allow,
            (Self::Rfc9309, RobotsAnswer::Html | RobotsAnswer::ClientError(_)) => {
                RobotsVerdict::Allow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_policies_differ_only_on_html_and_client_errors() {
        use RobotsAnswer as A;
        use RobotsVerdict as V;
        let rules = A::Rules("User-agent: *\nDisallow: /x\n".into());
        for policy in [RobotsPolicy::Strict, RobotsPolicy::Rfc9309] {
            assert_eq!(
                policy.verdict(&rules),
                V::Rules("User-agent: *\nDisallow: /x\n".into())
            );
            for code in [
                "robots_http_503",
                "robots_http_429",
                "timeout",
                "dns",
                "robots_redirect",
            ] {
                assert_eq!(
                    policy.verdict(&A::Unavailable(code.into())),
                    V::Unavailable(code.into())
                );
            }
        }
        let strict = RobotsPolicy::Strict;
        assert_eq!(
            strict.verdict(&A::Html),
            V::Unavailable("robots_html".into())
        );
        assert_eq!(strict.verdict(&A::ClientError(403)), V::DisallowAll);
        assert_eq!(strict.verdict(&A::ClientError(401)), V::DisallowAll);
        assert_eq!(strict.verdict(&A::ClientError(404)), V::Allow);
        let rfc = RobotsPolicy::Rfc9309;
        for answer in [
            A::Html,
            A::ClientError(401),
            A::ClientError(403),
            A::ClientError(404),
            A::ClientError(410),
        ] {
            assert_eq!(rfc.verdict(&answer), V::Allow, "{answer:?}");
        }
        assert_eq!(RobotsPolicy::default(), RobotsPolicy::Strict);
    }

    #[test]
    fn shared_hosting_never_widens_the_official_site() {
        // Ordinary institution: any host under the registrable domain.
        assert!(official_site_covers("www.busan.go.kr", "apply.busan.go.kr"));
        assert!(!official_site_covers("www.busan.go.kr", "seoul.go.kr"));
        // Path tenants: blog.naver.com/<agency> vouches for nothing.
        assert!(!official_site_covers("blog.naver.com", "form.naver.com"));
        assert!(!official_site_covers("blog.naver.com", "blog.naver.com"));
        assert!(!official_site_covers("sites.google.com", "docs.google.com"));
        assert!(!official_site_covers("cafe.daum.net", "cafe.daum.net"));
        // Subdomain tenants: only the exact tenant host.
        assert!(official_site_covers(
            "busan-youth.github.io",
            "busan-youth.github.io"
        ));
        assert!(!official_site_covers(
            "busan-youth.github.io",
            "attacker.github.io"
        ));
        assert!(!official_site_covers(
            "org.notion.site",
            "attacker.notion.site"
        ));
        assert!(!official_site_covers("github.io", "github.io"));
        assert!(is_shared_hosting("blog.naver.com"));
        assert!(is_shared_hosting("x.tistory.com"));
        assert!(!is_shared_hosting("www.busan.go.kr"));
        // Every listed platform resolves to itself as the registrable domain,
        // otherwise the list entry would never match.
        for domain in SHARED_HOSTING_SUBDOMAIN_TENANTS
            .iter()
            .chain(SHARED_HOSTING_PATH_TENANTS)
        {
            assert_eq!(
                registrable_domain(&format!("a.{domain}")).as_deref(),
                Some(*domain)
            );
        }
    }

    #[test]
    fn canonicalizes_https_and_removes_only_tracking_parameters() {
        let result = canonicalize_public_https_url(
            "https://brand.example/events?eventId=7&utm_source=x&t_click=y#top",
        )
        .unwrap();
        assert_eq!(
            result.canonical_url,
            "https://brand.example/events?eventId=7"
        );
    }

    #[test]
    fn canonical_key_sorts_query_pairs_and_trims_non_root_trailing_slash() {
        let unsorted =
            canonicalize_public_https_url("https://brand.example/events?b=1&a=2").unwrap();
        let sorted = canonicalize_public_https_url("https://brand.example/events?a=2&b=1").unwrap();
        assert_eq!(unsorted.canonical_url, sorted.canonical_url);
        assert_eq!(sorted.canonical_url, "https://brand.example/events?a=2&b=1");
        // The fetch URL keeps the published parameter order.
        assert_eq!(
            unsorted.url.as_str(),
            "https://brand.example/events?b=1&a=2"
        );

        let slash = canonicalize_public_https_url("https://brand.example/events/").unwrap();
        let bare = canonicalize_public_https_url("https://brand.example/events").unwrap();
        assert_eq!(slash.canonical_url, bare.canonical_url);
        assert_eq!(bare.canonical_url, "https://brand.example/events");
        assert_eq!(slash.url.as_str(), "https://brand.example/events/");

        let root = canonicalize_public_https_url("https://brand.example/").unwrap();
        assert_eq!(root.canonical_url, "https://brand.example/");
        assert_eq!(
            canonicalize_public_https_url("https://brand.example")
                .unwrap()
                .canonical_url,
            "https://brand.example/"
        );
        // Empty values and repeated keys survive the rebuild deterministically.
        let repeated = canonicalize_public_https_url("https://brand.example/x?k=2&k=1&e=").unwrap();
        assert_eq!(repeated.canonical_url, "https://brand.example/x?e=&k=1&k=2");
    }

    #[test]
    fn rejects_http_credentials_ips_and_local_hosts() {
        assert!(canonicalize_public_https_url("http://brand.example").is_err());
        assert!(canonicalize_public_https_url("https://u:p@brand.example").is_err());
        assert!(canonicalize_public_https_url("https://127.0.0.1/events").is_err());
        assert!(canonicalize_public_https_url("https://localhost/events").is_err());
    }

    #[test]
    fn registrable_domain_respects_korean_institutional_suffixes_and_flags_idn() {
        assert_eq!(
            registrable_domain("www.busan.go.kr").as_deref(),
            Some("busan.go.kr")
        );
        assert_eq!(
            registrable_domain("apply.busan.go.kr").as_deref(),
            Some("busan.go.kr")
        );
        assert_eq!(
            registrable_domain("form.naver.com").as_deref(),
            Some("naver.com")
        );
        assert_eq!(
            registrable_domain("Form.Example.ORG.").as_deref(),
            Some("example.org")
        );
        assert_eq!(registrable_domain("go.kr"), None);
        assert_eq!(registrable_domain("localhost"), None);
        assert_eq!(registrable_domain("127.0.0.1"), None);
        assert_eq!(registrable_domain("a..b"), None);
        // bokjiro-go-kr.evil.example is not within bokjiro.go.kr.
        assert_ne!(
            registrable_domain("bokjiro-go-kr.evil.example"),
            registrable_domain("www.bokjiro.go.kr")
        );
        assert!(has_punycode_label("xn--bokjiro-9ab.go.kr"));
        assert!(has_punycode_label("www.XN--80AK6AA92E.com"));
        assert!(!has_punycode_label("www.bokjiro.go.kr"));
    }

    #[test]
    fn domain_and_path_boundaries_are_exact() {
        assert!(host_is_within("www.brand.example", "brand.example", true));
        assert!(!host_is_within(
            "brand.example.attacker.test",
            "brand.example",
            true
        ));
        let paths = vec!["/events".to_string()];
        assert!(path_is_allowed("/events/7", &paths));
        assert!(!path_is_allowed("/eventstore", &paths));
    }

    #[test]
    fn classifies_event_indexes_and_details() {
        assert_eq!(
            classify_candidate_kind(&Url::parse("https://brand.example/events").unwrap()),
            CandidateKind::Index
        );
        assert_eq!(
            classify_candidate_kind(
                &Url::parse("https://brand.example/event/detail?eventId=7").unwrap()
            ),
            CandidateKind::Detail
        );
    }

    #[test]
    fn rejects_marketplace_product_details_without_blocking_product_documentation() {
        assert!(is_interactive_or_purchase_url(
            &Url::parse("https://shop.example/vp/products/1234").unwrap()
        ));
        assert!(is_interactive_or_purchase_url(
            &Url::parse("https://shop.example/product/detail?id=1234").unwrap()
        ));
        assert!(!is_interactive_or_purchase_url(
            &Url::parse("https://www.microsoft.com/ko-kr/education/products/office").unwrap()
        ));
    }
}
