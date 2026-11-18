//! The one vocabulary of link labels and page structure that the "신청"
//! link resolver and the apply-link reader share.
//!
//! Labels: what an element's accessible name (text, `aria-label`, `title`,
//! image `alt`) says it does. A name *applies* when it matches an
//! [`Meaning::Apply`] entry and no [`Meaning::AboutApplication`] or
//! [`Meaning::SiteService`] entry. Structure: class/id tokens of page chrome
//! (menus, headers, footers) and of primary content, and field names of a
//! search box. Everything else the reader decides from element structure
//! (semantic elements, ARIA roles, forms, repetition), not from words.
//!
//! Adding an entry changes both consumers; the tests below pin each table.
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meaning {
    /// Goes to an application step: 신청하기, 온라인 신청, 접수하기.
    Apply,
    /// About an application, not to it: 안내, 방법, 결과 조회, 취소.
    AboutApplication,
    /// A site-wide service that also says 신청 (민원, 대관, 회원 ...).
    SiteService,
}

/// Label vocabulary: regex fragments, matched anywhere in the name.
pub const LABELS: &[(&str, Meaning)] = &[
    (r"신청하기", Meaning::Apply),
    (r"온라인\s*신청", Meaning::Apply),
    (r"신청\s*바로\s*가기", Meaning::Apply),
    (r"접수하기", Meaning::Apply),
    (r"안내", Meaning::AboutApplication),
    (r"절차", Meaning::AboutApplication),
    (r"유의", Meaning::AboutApplication),
    (r"방법", Meaning::AboutApplication),
    (r"조회", Meaning::AboutApplication),
    (r"확인", Meaning::AboutApplication),
    (r"결과", Meaning::AboutApplication),
    (r"목록", Meaning::AboutApplication),
    (r"현황", Meaning::AboutApplication),
    (r"취소", Meaning::AboutApplication),
    (r"변경", Meaning::AboutApplication),
    (r"수정", Meaning::AboutApplication),
    (r"내역", Meaning::AboutApplication),
    (r"민원", Meaning::SiteService),
    (r"문자", Meaning::SiteService),
    (r"알림", Meaning::SiteService),
    (r"여권", Meaning::SiteService),
    (r"정보공개", Meaning::SiteService),
    (r"구독", Meaning::SiteService),
    (r"희망도서", Meaning::SiteService),
    (r"대관", Meaning::SiteService),
    (r"견학", Meaning::SiteService),
    (r"상담", Meaning::SiteService),
    (r"실명제", Meaning::SiteService),
    (r"기부", Meaning::SiteService),
    (r"회원", Meaning::SiteService),
    (r"뉴스레터", Meaning::SiteService),
    (r"설문", Meaning::SiteService),
];

fn alternation(meaning: Meaning) -> Regex {
    let parts: Vec<&str> = LABELS
        .iter()
        .filter(|(_, m)| *m == meaning)
        .map(|(p, _)| *p)
        .collect();
    Regex::new(&parts.join("|")).expect("vocabulary pattern is valid")
}

pub(crate) static APPLY: LazyLock<Regex> = LazyLock::new(|| alternation(Meaning::Apply));
pub(crate) static ABOUT_APPLICATION: LazyLock<Regex> =
    LazyLock::new(|| alternation(Meaning::AboutApplication));
pub(crate) static SITE_SERVICE: LazyLock<Regex> =
    LazyLock::new(|| alternation(Meaning::SiteService));

/// The accessible name says the element goes to an application step.
pub fn names_application(name: &str) -> bool {
    APPLY.is_match(name) && !ABOUT_APPLICATION.is_match(name) && !SITE_SERVICE.is_match(name)
}

/// class/id tokens (split on `-`, `_`, space) of page chrome: a link inside
/// is site navigation, never a programme's own button. Semantic elements
/// (`header`, `nav`, `footer`, `aside`) and ARIA landmark roles count too;
/// those are structure, not vocabulary.
pub const CHROME_TOKENS: &[&str] = &[
    "gnb",
    "lnb",
    "snb",
    "tnb",
    "nav",
    "navi",
    "menu",
    "allmenu",
    "header",
    "footer",
    "quick",
    "sitemap",
    "breadcrumb",
    "util",
    "skip",
];

/// class/id tokens of a page's primary content area.
pub const CONTENT_TOKENS: &[&str] = &[
    "content", "contents", "cont", "conts", "article", "board", "bbs", "view", "detail",
];

/// `name`/`id`/`title` tokens of a site search box: its form is not an
/// application form.
pub const SEARCH_FIELD_TOKENS: &[&str] = &[
    "search",
    "keyword",
    "keywords",
    "query",
    "srch",
    "검색",
    "검색어",
];

/// Lower-case tokens of a class or id attribute value.
pub fn tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
}

pub fn any_token_in(value: &str, table: &[&str]) -> bool {
    tokens(value).any(|t| table.contains(&t.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_names_are_actions_not_information_or_site_services() {
        for name in [
            "신청하기",
            "온라인 신청",
            "온라인신청 바로가기",
            "신청 바로가기",
            "신청바로가기",
            "접수하기",
            "지원사업 신청하기",
        ] {
            assert!(names_application(name), "{name}");
        }
        for name in [
            "온라인신청 절차 및 유의사항",
            "신청확인",
            "민원 신청하기",
            "대관 신청하기",
            "신청 결과 조회",
            "신청방법 안내",
            "신청한 정책보기",
            "신청자격 검증하기",
            "공간대관신청",
            "지원하기",
            "회원가입 후 신청하기",
        ] {
            assert!(!names_application(name), "{name}");
        }
    }

    #[test]
    fn every_entry_compiles_and_belongs_to_one_meaning() {
        for (pattern, _) in LABELS {
            Regex::new(pattern).unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        for (pattern, _) in LABELS {
            assert!(seen.insert(*pattern), "duplicate {pattern}");
        }
    }

    #[test]
    fn structure_tokens_are_whole_tokens() {
        assert!(any_token_in("div gnb-wrap", CHROME_TOKENS));
        assert!(any_token_in("mo-tab-bar nav", CHROME_TOKENS));
        assert!(any_token_in("allmenu-menu", CHROME_TOKENS));
        // Inside other words a token does not count.
        assert!(!any_token_in("navigator-free menus2", CHROME_TOKENS));
        assert!(!any_token_in("right-fix-btn", CHROME_TOKENS));
        assert!(any_token_in("tab-conts mt30", CONTENT_TOKENS));
        assert!(any_token_in("board-view scroll-check", CONTENT_TOKENS));
        assert!(!any_token_in("container", CONTENT_TOKENS));
        // youth.seoul.go.kr's search box: name="keyWord".
        assert!(any_token_in("keyWord", SEARCH_FIELD_TOKENS));
        assert!(any_token_in("sc_keyword", SEARCH_FIELD_TOKENS));
    }
}
