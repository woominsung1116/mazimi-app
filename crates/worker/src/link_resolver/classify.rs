//! Link classes a–e from one URL and what a check saw of it.
//!
//! A rule-for-rule port of the deep-link audit's classifier: the same
//! regular expressions, the same order of tests and the same sub-reasons, so
//! the offline replay over the audit's production data can be compared with
//! the audit row by row (`replay_tests`). Policy that the audit did not have
//! (origin rule, robots cap, soft failures, login access) lives in
//! [`super::evaluate`], not here.
use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// a < b < c < d in preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// a: an application form or an application path of this programme.
    Application,
    /// b: the programme's own notice or page at the agency.
    Notice,
    /// c: a portal information page (정부24, 복지로, 온통청년 detail).
    Information,
    /// d: a homepage, a list or search page, or a page several programmes share.
    Portal,
}

impl Kind {
    pub fn rank(self) -> i32 {
        match self {
            Self::Application => 0,
            Self::Notice => 1,
            Self::Information => 2,
            Self::Portal => 3,
        }
    }
    pub fn letter(self) -> char {
        ['a', 'b', 'c', 'd'][self.rank() as usize]
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Notice => "notice",
            Self::Information => "information",
            Self::Portal => "portal",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "application" => Self::Application,
            "notice" => Self::Notice,
            "information" => Self::Information,
            "portal" => Self::Portal,
            _ => return None,
        })
    }
}

/// What a fetched HTML page showed. Bounded: a title of at most 150
/// characters and at most 12 candidate links (see [`super::page::analyze`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageFacts {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub forms: u32,
    #[serde(default)]
    pub inputs: u32,
    #[serde(default)]
    pub textareas: u32,
    #[serde(default)]
    pub selects: u32,
    #[serde(default)]
    pub password: bool,
    /// (link text, href as written on the page) for links whose text names
    /// an application step.
    #[serde(default)]
    pub apply_links: Vec<(String, String)>,
}

/// One check of one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// Not fetched: robots.txt disallows the URL or could not be read.
    Blocked,
    /// No HTTP response. `code` is `dns`, `timeout`, `connect`, `tls`,
    /// `redirect_loop`, `unsafe` or `body`.
    Error { code: String },
    /// An HTTP response after following redirects.
    Fetched {
        status: u16,
        final_url: String,
        page: Option<PageFacts>,
    },
}

/// The audit's verdict for one URL: class, sub-reason and the URL the
/// check ended on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    Usable {
        kind: Kind,
        sub: String,
        final_url: String,
    },
    /// e: the URL does not lead to a usable page.
    Broken { sub: String },
}

impl Class {
    pub fn letter(&self) -> char {
        match self {
            Self::Usable { kind, .. } => kind.letter(),
            Self::Broken { .. } => 'e',
        }
    }
    pub fn sub(&self) -> &str {
        match self {
            Self::Usable { sub, .. } | Self::Broken { sub } => sub,
        }
    }
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("link classifier pattern is valid")
}

static GENERIC_FILE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"/(main|index)(/main)?\.(do|asp|aspx|jsp|php|html?|nm|es)$|/main$|/index$|/hmpg/main")
});
static SECTION_HOME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(/[a-z0-9_-]+)?(/index|/main)?(\.(do|jsp|php|asp|html?|nm|es|iksan|wanju|goseong|9is))?$")
});
static LIST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)/notice/gosi$|list\.(do|jsp|php|asp|es|tc)|/list\b|boardlist|bbslist|nttlist|selectbbsnttlist|/board\.es$",
    )
});
static VIEW: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)view|nttid|ntt_?no|nttsn|articleno|artcl|dataUid|idx=|seq=|no=\d|detail|read\.|bbsidx|board_?seq|wr_id|policy/detail|bbs_?no|gosi",
    )
});
// Python's `(?<![a-z])word(?![a-z])` without look-around: a letter on
// either side is consumed instead, which is equivalent for `is_match`.
static APPLY: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:^|[^a-z])(?:apply|aply|rcept|reqst|request|applicat|application|sinchung|reserv|reservation|recruit|enroll|forms?|survey|submit|appl|booking)(?:[^a-z]|$)|aply|apply|rcept|reqst",
    )
});
static STRONG_APPLY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)reqst|aply|apply|rcept|reserv|/online\.do"));
static SEARCH: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)search"));
static TITLE_CLOSED: LazyLock<Regex> = LazyLock::new(|| re(r"모집\s*종료|마감"));
static TITLE_CLOSED_FORM: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)모집\s*종료|접수\s*마감|마감되었|종료되었|closed"));
static TITLE_NOT_APPLY: LazyLock<Regex> = LazyLock::new(|| re(r"민원|여권|정보공개|마감|종료"));
/// Link text of an application step (audit `hop`).
pub(crate) static HOP_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"신청하기|온라인\s*신청|신청\s*바로가기|접수하기|참여\s*접수|지원하기|신청서\s*작성")
});
/// Site-wide services that also say 신청 (민원, 대관, 회원 ...): the shared
/// table in [`super::vocabulary`].
pub(crate) use super::vocabulary::SITE_SERVICE as NAV_TEXT;

/// Exact homepage paths (no query) of Korean public sites.
const GENERIC_PATHS: &[&str] = &[
    "",
    "/index.html",
    "/index.htm",
    "/index.do",
    "/main",
    "/main.do",
    "/main/main.do",
    "/index.jsp",
    "/index.php",
    "/main.php",
    "/index.asp",
    "/default.asp",
    "/home",
    "/kor",
    "/ko",
    "/kr",
    "/web/main",
    "/www",
    "/portal",
    "/user/main.do",
    "/index.nm",
    "/main/index.do",
    "/kor/main.do",
    "/ko/main.do",
    "/kr/main.do",
    "/main/main.php",
    "/site/main",
    "/index",
];
/// Hosts whose short links or pages need a login when the form is gated.
const FORM_HOST_HINTS: &[&str] = &[
    "docs.google.com",
    "forms.gle",
    "form.naver.com",
    "naver.me",
    "forms.office.com",
    "tally.so",
    "typeform.com",
    "survey",
    "smore",
];
const ONLINE_FORM_HOSTS: &[&str] = &[
    "docs.google.com",
    "apply.do",
    "form.naver.com",
    "forms.office.com",
    "tally.so",
    "typeform.com",
    "smore",
    "modoo.at/form",
];

pub(crate) fn host_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// Portal information pages: never fetched, always class c.
pub fn portal_kind(url: &str) -> Option<&'static str> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let path = parsed.path();
    match host {
        "youthcenter.go.kr" if path.contains("ythPlcyDetail") => Some("youthcenter_detail"),
        "gov.kr" if path.contains("rcvfvrSvc") => Some("gov24_info"),
        "plus.gov.kr" if path.contains("benefitV2") => Some("gov24_info"),
        "bokjiro.go.kr" if path.contains("wlfareInfo") => Some("bokjiro_info"),
        _ => None,
    }
}

/// A homepage or a section home, extended beyond
/// [`mazimi_core::application_resources::is_generic_portal`]: any
/// `/main.*` or `/index.*` page without an article key, and a one-segment
/// section path such as `/youth`.
pub fn is_generic(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let path = parsed.path().trim_end_matches('/').to_lowercase();
    let query = parsed.query().unwrap_or("");
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(k, v)| (k.to_lowercase(), v.into_owned()))
        .collect();
    let article_key = pairs.iter().any(|(k, v)| {
        !v.is_empty()
            && matches!(
                k.as_str(),
                "list_no" | "nttid" | "nttno" | "idx" | "seq" | "no" | "bizid"
            )
    });
    if GENERIC_FILE.is_match(&path)
        && !article_key
        && !query.contains("menuCd")
        && !query.contains("mid=")
    {
        return true;
    }
    let neutral = pairs.iter().all(|(k, _)| {
        matches!(
            k.as_str(),
            "lang" | "language" | "locale" | "hl" | "source" | "ref" | "campaign" | "siteid"
        ) || k.starts_with("utm_")
    });
    // An in-page anchor (`#mltm`, `#content`) is not navigation state; an
    // SPA route (`#/apply/42`, `#!/x`) or a parameter fragment is.
    let no_fragment = parsed
        .fragment()
        .is_none_or(|f| !f.starts_with('/') && !f.starts_with('!') && !f.contains('='));
    if SECTION_HOME.is_match(&path) && neutral && no_fragment && path.matches('/').count() <= 1 {
        return true;
    }
    (GENERIC_PATHS.contains(&path.as_str()) && pairs.is_empty())
        || mazimi_core::application_resources::is_generic_portal(url)
}

/// `board.es?` not followed (anywhere later) by `list_no`.
fn board_without_article(path_q: &str) -> bool {
    let lower = path_q.to_lowercase();
    lower
        .rfind("board.es?")
        .is_some_and(|i| !lower[i..].contains("list_no"))
}

fn is_list(path_q: &str) -> bool {
    LIST.is_match(path_q) || board_without_article(path_q)
}

/// `신청` not followed by `안내`, or `접수` not followed by `마감`
/// (whitespace allowed between).
fn title_names_application(title: &str) -> bool {
    let followed_by = |word: &str, next: &str| {
        title
            .match_indices(word)
            .any(|(i, _)| !title[i + word.len()..].trim_start().starts_with(next))
    };
    followed_by("신청", "안내") || followed_by("접수", "마감")
}

fn path_and_query(url: &str) -> (String, String) {
    match Url::parse(url) {
        Ok(parsed) => (
            parsed.path().to_string(),
            format!("{}?{}", parsed.path(), parsed.query().unwrap_or("")),
        ),
        Err(_) => (String::new(), "?".into()),
    }
}

/// Context the class of a URL depends on beyond the URL itself.
pub struct Context<'a> {
    /// Two or more differently titled programmes list this URL.
    pub shared: bool,
    /// Pages on the request host that carry a link with this href.
    pub nav_count: &'a dyn Fn(&str) -> usize,
}

/// The audit's `classify_url`. `observation` is `None` when the URL was
/// never fetched: the class then rests on the URL pattern alone.
pub fn classify(url: &str, observation: Option<&Observation>, ctx: &Context<'_>) -> Class {
    let usable = |kind, sub: &str, final_url: &str| Class::Usable {
        kind,
        sub: sub.to_string(),
        final_url: final_url.to_string(),
    };
    let broken = |sub: String| Class::Broken { sub };
    if let Some(portal) = portal_kind(url) {
        return usable(Kind::Information, portal, url);
    }
    let mut final_url = url.to_string();
    let mut page: Option<&PageFacts> = None;
    match observation {
        None | Some(Observation::Blocked) => {}
        Some(Observation::Error { code }) => {
            return broken(if code == "dns" {
                "dns".into()
            } else {
                format!("unreachable:{code}")
            })
        }
        Some(Observation::Fetched {
            status,
            final_url: fetched,
            page: facts,
        }) => {
            final_url = fetched.clone();
            page = facts.as_ref();
            if *status >= 400 {
                return broken(format!("http_{status}"));
            }
            if final_url.contains("closedform") {
                return broken("form_closed".into());
            }
            // Deviation from the audit: login pages are matched on path
            // segments, query keys and sign-in hosts (`/member/login.do`,
            // `?loginMode=login`), not on substrings (`statHtml.do?sso=ok` is a
            // statistics page), and a URL that itself names a login page is
            // not exempt (`...&login=login` served a sign-in form).
            if is_login_url(&final_url) {
                return broken(if FORM_HOST_HINTS.iter().any(|h| url.contains(h)) {
                    "form_behind_login".into()
                } else {
                    "login_wall".into()
                });
            }
            if is_generic(&final_url) && !is_generic(url) {
                return broken("redirect_to_home".into());
            }
            if let Some(portal) = portal_kind(&final_url) {
                return usable(Kind::Information, portal, &final_url);
            }
        }
    }
    let empty = PageFacts::default();
    let page = page.unwrap_or(&empty);
    let title = page.title.as_str();
    if title.contains("관리자") || final_url.contains("manageYthPlcy") {
        return broken("admin_page".into());
    }
    let host = host_of(&final_url);
    let (path, path_q) = path_and_query(&final_url);
    let form_vendor = ["google", "naver", "apply.do"]
        .iter()
        .any(|h| host.contains(h));
    if TITLE_CLOSED.is_match(title) && form_vendor {
        return broken("form_closed".into());
    }
    if (ONLINE_FORM_HOSTS.iter().any(|h| host.contains(h)) && final_url.contains("viewform"))
        || final_url.contains("form.naver.com/response")
    {
        return usable(Kind::Application, "online_form", &final_url);
    }
    if is_generic(&final_url) || is_generic(url) {
        return usable(Kind::Portal, "homepage", &final_url);
    }
    if is_list(&path_q) || SEARCH.is_match(&path) {
        return usable(Kind::Portal, "list_page", &final_url);
    }
    let form_signal =
        page.forms > 0 && page.inputs + page.textareas + page.selects >= 4 && !page.password;
    if APPLY.is_match(&path_q) && (STRONG_APPLY.is_match(&path_q) || !VIEW.is_match(&path_q)) {
        if ctx.shared {
            return usable(Kind::Portal, "shared_apply_portal", &final_url);
        }
        let sub = if form_signal {
            "apply_path+form"
        } else {
            "apply_path"
        };
        return usable(Kind::Application, sub, &final_url);
    }
    if TITLE_CLOSED_FORM.is_match(title) && form_vendor {
        return broken("form_closed".into());
    }
    if title_names_application(title) && !TITLE_NOT_APPLY.is_match(title) && !ctx.shared {
        return usable(Kind::Application, "title_apply_page", &final_url);
    }
    let hop = page
        .apply_links
        .iter()
        .any(|(text, href)| is_hop_text(text) && (ctx.nav_count)(href) < 3);
    let with_hop = |sub: &str| {
        if hop {
            format!("{sub}+apply_link")
        } else {
            sub.to_string()
        }
    };
    if VIEW.is_match(&path_q) {
        if ctx.shared {
            return usable(Kind::Portal, "shared_notice", &final_url);
        }
        return usable(Kind::Notice, &with_hop("notice_view"), &final_url);
    }
    if ctx.shared {
        return usable(Kind::Portal, "shared_page", &final_url);
    }
    usable(Kind::Notice, &with_hop("agency_program_page"), &final_url)
}

/// Link text of an application step that is not a site-wide service (the
/// audit's `hop` annotation).
pub fn is_hop_text(text: &str) -> bool {
    HOP_TEXT.is_match(text) && !NAV_TEXT.is_match(text)
}

static LOGIN_TITLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"로그인[^.]{0,12}(후|필요|하셔야|하신|해\s*주)"));

/// Design 5.8 link text: 신청하기, 온라인 신청, 신청 바로가기, 접수하기,
/// and not a site-wide service or a page about the application.
pub fn is_promotable_hop_text(text: &str) -> bool {
    super::vocabulary::names_application(text)
}

/// A page whose title says it needs a login first ("로그인 후 신청할 수
/// 있습니다"): a login gate served without a redirect.
pub fn title_is_login_gate(title: &str) -> bool {
    LOGIN_TITLE.is_match(title)
}

/// The URL itself names an application step (`reqst`, `aply`, `apply`,
/// `rcept`, `reserv`, `/online.do`).
pub fn has_strong_apply_path(url: &str) -> bool {
    STRONG_APPLY.is_match(&path_and_query(url).1)
}

/// A sign-in page by its URL: a path segment naming a login (`login`,
/// `logon`, `signin`, or exactly `sso`, `auth`, `oauth`), a `login` or
/// `loginMode` query key, or a sign-in host.
pub fn is_login_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if matches!(
        host.as_str(),
        "accounts.google.com" | "nid.naver.com" | "logins.daum.net" | "accounts.kakao.com"
    ) {
        return true;
    }
    let segment = parsed.path_segments().into_iter().flatten().any(|seg| {
        let seg = seg.to_ascii_lowercase();
        ["login", "logon", "signin", "sign-in"]
            .iter()
            .any(|w| seg.contains(w))
            || matches!(seg.as_str(), "sso" | "auth" | "oauth" | "oauth2")
    });
    segment
        || parsed.query_pairs().any(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k == "login" || k == "loginmode"
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> usize {
        0
    }
    fn ctx() -> Context<'static> {
        Context {
            shared: false,
            nav_count: &none,
        }
    }
    fn fetched(final_url: &str, title: &str) -> Observation {
        Observation::Fetched {
            status: 200,
            final_url: final_url.into(),
            page: Some(PageFacts {
                title: title.into(),
                ..PageFacts::default()
            }),
        }
    }
    fn class(url: &str, obs: Option<&Observation>) -> (char, String) {
        let c = classify(url, obs, &ctx());
        (c.letter(), c.sub().to_string())
    }

    #[test]
    fn portal_pages_are_information_without_a_fetch() {
        assert_eq!(
            class(
                "https://www.gov.kr/portal/rcvfvrSvc/dtlEx/305000000174",
                None
            ),
            ('c', "gov24_info".into())
        );
        assert_eq!(
            class("https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/20261019005400214010", None).0,
            'c'
        );
        // A lookalike host is not a portal.
        assert_ne!(
            class("https://www.gov.kr.evil.example/rcvfvrSvc/1", None).0,
            'c'
        );
    }

    #[test]
    fn homepages_and_section_homes_are_generic() {
        for url in [
            "https://www.yeonggwang.go.kr/main",
            "https://www.jeonju.go.kr/index.9is",
            "https://www.kosaf.go.kr/ko/main.do?currentMain=1",
            "https://www.hifive.go.kr/",
            "https://www.wanju.go.kr/wjyouth",
            "https://www.example.go.kr/index.jsp",
        ] {
            assert!(is_generic(url), "{url}");
        }
        assert!(is_generic("https://www.molit.go.kr/portal.do#mltm"));
        for url in [
            "https://www.example.go.kr/main.do?nttId=3",
            "https://www.example.go.kr/youth/policy/view.do?idx=3",
            "https://www.example.go.kr/youth#/apply/7",
            "https://www.example.go.kr/youth#step=2",
        ] {
            assert!(!is_generic(url), "{url}");
        }
    }

    /// The audit's broken examples, by rule.
    #[test]
    fn closed_forms_admin_pages_and_dead_links_are_broken() {
        let closed = Observation::Fetched {
            status: 200,
            final_url: "https://docs.google.com/forms/d/e/1FAIpQL/closedform".into(),
            page: None,
        };
        assert_eq!(
            class("https://forms.gle/35Dr51sLkNxSBvBH8", Some(&closed)),
            ('e', "form_closed".into())
        );
        let admin = "https://www.youthcenter.go.kr/manageYthPlcy/ythPlcyManageMain/ythPlcyManageRegistry/ypmRegistryStep01";
        assert_eq!(
            class(admin, Some(&fetched(admin, "청년정책 관리"))),
            ('e', "admin_page".into())
        );
        assert_eq!(
            class(
                "https://www.example.go.kr/youth/notice/view.do?idx=3",
                Some(&fetched("https://www.example.go.kr/main.do", "기관"))
            ),
            ('e', "redirect_to_home".into())
        );
        let gone = Observation::Fetched {
            status: 404,
            final_url: "https://www.example.go.kr/x".into(),
            page: None,
        };
        assert_eq!(
            class("https://www.example.go.kr/x", Some(&gone)).1,
            "http_404"
        );
        let login = fetched(
            "https://www.example.go.kr/member/login.do?return=/x",
            "로그인",
        );
        assert_eq!(
            class(
                "https://www.example.go.kr/youth/notice/view.do?idx=3",
                Some(&login)
            )
            .1,
            "login_wall"
        );
    }

    #[test]
    fn application_notice_and_portal_classes_follow_the_audit_order() {
        let form = "https://docs.google.com/forms/d/e/1FAIpQL/viewform";
        assert_eq!(class(form, Some(&fetched(form, "신청서"))).1, "online_form");
        let apply = "https://www.example.go.kr/youth/aplyReqst.do?bizId=7";
        assert_eq!(class(apply, Some(&fetched(apply, "청년"))).1, "apply_path");
        let notice = "https://www.wonju.go.kr/www/selectBbsNttView.do?key=313&nttNo=474656";
        assert_eq!(
            class(notice, Some(&fetched(notice, "청년도전지원사업"))),
            ('b', "notice_view".into())
        );
        let list = "https://www.example.go.kr/board/selectBbsNttList.do?bbsNo=1";
        assert_eq!(class(list, Some(&fetched(list, "공지"))).1, "list_page");
        let title = "https://www.example.go.kr/youth/program1";
        assert_eq!(
            class(title, Some(&fetched(title, "청년 월세 신청"))),
            ('a', "title_apply_page".into())
        );
        assert_eq!(
            class(title, Some(&fetched(title, "청년 월세 신청 안내"))),
            ('b', "agency_program_page".into())
        );
        let shared = Context {
            shared: true,
            nav_count: &none,
        };
        assert_eq!(
            classify(notice, Some(&fetched(notice, "공고")), &shared).sub(),
            "shared_notice"
        );
    }

    #[test]
    fn application_links_on_a_notice_count_unless_they_repeat_across_the_host() {
        let notice = "https://www.example.go.kr/youth/notice/view.do?idx=3";
        let page = Observation::Fetched {
            status: 200,
            final_url: notice.into(),
            page: Some(PageFacts {
                title: "청년 지원 공고".into(),
                apply_links: vec![
                    ("온라인 신청하기".into(), "/apply/7".into()),
                    ("민원 신청".into(), "/minwon".into()),
                ],
                ..PageFacts::default()
            }),
        };
        assert_eq!(class(notice, Some(&page)).1, "notice_view+apply_link");
        let everywhere = |_: &str| 3;
        let nav = Context {
            shared: false,
            nav_count: &everywhere,
        };
        assert_eq!(classify(notice, Some(&page), &nav).sub(), "notice_view");
    }

    #[test]
    fn promotable_link_texts_go_to_the_application_not_about_it() {
        for text in [
            "신청하기",
            "온라인 신청",
            "온라인신청 바로가기",
            "신청 바로가기",
            "접수하기",
            "지원사업 신청하기",
        ] {
            assert!(is_promotable_hop_text(text), "{text}");
        }
        for text in [
            "온라인신청 절차 및 유의사항",
            "신청확인",
            "민원 신청하기",
            "대관 신청하기",
            "신청 결과 조회",
            "지원하기",
            "신청방법 안내",
        ] {
            assert!(!is_promotable_hop_text(text), "{text}");
        }
        assert!(title_is_login_gate("로그인을 하신 후에 이용 가능합니다."));
        assert!(title_is_login_gate(
            "로그인 후에 신청할 수 있습니다.오류에 의한 페이지 자동 이동"
        ));
        assert!(!title_is_login_gate("청년 월세 지원 신청"));
    }

    #[test]
    fn title_application_words_respect_their_exclusions() {
        assert!(title_names_application("신청하기"));
        assert!(!title_names_application("신청 안내"));
        assert!(title_names_application("접수 기간"));
        assert!(!title_names_application("접수  마감"));
        assert!(title_names_application("신청 안내 및 신청서"));
    }
}
