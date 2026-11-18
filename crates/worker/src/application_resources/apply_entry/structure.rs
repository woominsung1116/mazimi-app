//! Reading a page's own application element and recognising an application
//! step, from element structure. Pure functions over HTML.
//!
//! An element *applies* when its accessible name means apply (the shared
//! table [`crate::link_resolver::vocabulary`]). Among those, page chrome is
//! dropped (semantic `header`/`nav`/`footer`/`aside`, ARIA landmark roles,
//! chrome class/id tokens) and the rest is scored by where it sits: in the
//! primary content, inside a non-search form, styled as a button, repeated
//! across the page (menus repeat), pointing at an application path.
use crate::link_resolver::vocabulary::{
    any_token_in, names_application, CHROME_TOKENS, CONTENT_TOKENS, SEARCH_FIELD_TOKENS,
};
use regex::Regex;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};
use std::{collections::HashMap, sync::LazyLock};

/// A score at least this high is the page's own application element.
pub const MIN_SCORE: i32 = 3;
const MAX_ELEMENTS: usize = 3000;

/// One element whose accessible name means apply, with its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub url: String,
    /// Accessible name, whitespace collapsed, at most 80 characters.
    pub label: String,
    pub score: i32,
    /// The structural reasons, for evidence (`content+form+button`).
    pub why: String,
}

static ONCLICK_URL: LazyLock<Regex> = LazyLock::new(|| {
    // location.href='...', location='...', window.open('...'), location.assign('...')
    Regex::new(r#"(?:location(?:\.href)?\s*=\s*|window\.open\s*\(\s*|location\.(?:assign|replace)\s*\(\s*)['"]([^'"]{1,500})['"]"#)
        .expect("valid")
});
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));
static STRONG_APPLY_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)reqst|aply|apply|rcept|/step\d").expect("valid"));

fn collapse(text: &str) -> String {
    SPACES.replace_all(text.trim(), " ").into_owned()
}

/// Text content with image `alt` text, else `aria-label`, `title`, `value`.
fn accessible_name(element: &ElementRef<'_>) -> String {
    let mut text = String::new();
    for node in element.descendants() {
        if let Some(t) = node.value().as_text() {
            text.push_str(t);
        } else if let Some(e) = node.value().as_element() {
            if e.name() == "img" {
                if let Some(alt) = e.attr("alt") {
                    text.push(' ');
                    text.push_str(alt);
                }
            }
        }
    }
    let mut name = collapse(&text);
    for attr in ["aria-label", "title", "value"] {
        if !name.is_empty() {
            break;
        }
        name = collapse(element.value().attr(attr).unwrap_or(""));
    }
    name.chars().take(80).collect()
}

/// Where the element sends the user: `href` (not a script or an in-page
/// anchor), or a URL literal in `onclick`.
fn target(element: &ElementRef<'_>, base: &Url) -> Option<Url> {
    let value = element.value();
    let raw = value
        .attr("href")
        .map(str::trim)
        .filter(|h| {
            let lower = h.to_ascii_lowercase();
            !h.is_empty() && !h.starts_with('#') && !lower.starts_with("javascript:")
        })
        .map(str::to_owned)
        .or_else(|| {
            value
                .attr("onclick")
                .and_then(|js| ONCLICK_URL.captures(js))
                .map(|c| c[1].to_string())
        })?;
    if raw.contains("${") || raw.contains("{{") {
        return None;
    }
    let url = base.join(&raw).ok()?;
    matches!(url.scheme(), "https" | "http").then_some(url)
}

fn role(element: &ElementRef<'_>) -> String {
    element
        .value()
        .attr("role")
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn class_and_id(element: &ElementRef<'_>) -> String {
    format!(
        "{} {}",
        element.value().attr("class").unwrap_or(""),
        element.value().attr("id").unwrap_or("")
    )
}

/// The element or an ancestor is page chrome.
fn in_chrome(element: &ElementRef<'_>) -> bool {
    std::iter::once(*element)
        .chain(element.ancestors().filter_map(ElementRef::wrap))
        .any(|e| {
            matches!(e.value().name(), "header" | "nav" | "footer" | "aside")
                || matches!(
                    role(&e).as_str(),
                    "navigation" | "banner" | "contentinfo" | "complementary" | "menu" | "menubar"
                )
                || any_token_in(&class_and_id(&e), CHROME_TOKENS)
        })
}

fn in_content(element: &ElementRef<'_>) -> bool {
    element.ancestors().filter_map(ElementRef::wrap).any(|e| {
        matches!(e.value().name(), "main" | "article")
            || role(&e) == "main"
            || any_token_in(&class_and_id(&e), CONTENT_TOKENS)
    })
}

fn in_application_form(element: &ElementRef<'_>) -> bool {
    element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .find(|e| e.value().name() == "form")
        .is_some_and(|form| !search_form(&form))
}

fn button_like(element: &ElementRef<'_>) -> bool {
    element.value().name() == "button"
        || role(element) == "button"
        || element
            .value()
            .attr("class")
            .is_some_and(|c| any_token_in(c, &["btn", "button"]))
}

/// The page's elements whose name means apply, best first; page chrome and
/// targets that are the page itself are dropped. Scores below
/// [`MIN_SCORE`] are kept for evidence but are not the page's own element.
pub fn apply_elements(page_url: &str, html: &str) -> Vec<Found> {
    let Ok(base) = Url::parse(page_url) else {
        return vec![];
    };
    let doc = Html::parse_document(html);
    let selector =
        Selector::parse("a, button, [role=button], input[type=button], input[type=submit]")
            .expect("valid selector");
    let elements: Vec<ElementRef<'_>> = doc.select(&selector).take(MAX_ELEMENTS).collect();
    // How often each target occurs on the page: menus repeat their links.
    let mut repeats: HashMap<String, usize> = HashMap::new();
    for element in &elements {
        if let Some(url) = target(element, &base) {
            *repeats.entry(without_fragment(&url)).or_default() += 1;
        }
    }
    let mut found: Vec<Found> = vec![];
    for element in &elements {
        let label = accessible_name(element);
        if !names_application(&label) || in_chrome(element) {
            continue;
        }
        let Some(url) = target(element, &base) else {
            continue;
        };
        if without_fragment(&url) == without_fragment(&base) {
            continue;
        }
        let mut score = 0;
        let mut why: Vec<&str> = vec![];
        if in_content(element) {
            score += 3;
            why.push("content");
        }
        if in_application_form(element) {
            score += 2;
            why.push("form");
        }
        if button_like(element) {
            score += 1;
            why.push("button");
        }
        if STRONG_APPLY_PATH.is_match(&format!("{}?{}", url.path(), url.query().unwrap_or(""))) {
            score += 1;
            why.push("apply_path");
        }
        if repeats.get(&without_fragment(&url)).copied().unwrap_or(0) >= 3 {
            score -= 3;
            why.push("repeated");
        }
        let url = url.to_string();
        match found.iter_mut().find(|f| f.url == url) {
            Some(f) if f.score >= score => {}
            Some(f) => {
                f.score = score;
                f.why = why.join("+");
                f.label = label;
            }
            None => found.push(Found {
                url,
                label,
                score,
                why: why.join("+"),
            }),
        }
    }
    found.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
    found
}

fn without_fragment(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

/// The single best element at or above [`MIN_SCORE`], or every element
/// tied at the top when there are several (the caller resolves the tie).
pub fn best(found: &[Found]) -> Vec<&Found> {
    let Some(top) = found.first().filter(|f| f.score >= MIN_SCORE) else {
        return vec![];
    };
    found.iter().filter(|f| f.score == top.score).collect()
}

/// A field that a site search box carries.
fn search_field(field: &ElementRef<'_>) -> bool {
    let v = field.value();
    v.attr("type")
        .is_some_and(|t| t.eq_ignore_ascii_case("search"))
        || ["name", "id", "title"]
            .iter()
            .filter_map(|a| v.attr(a))
            .any(|value| any_token_in(value, SEARCH_FIELD_TOKENS))
}

fn search_form(form: &ElementRef<'_>) -> bool {
    let fields = Selector::parse("input, select, textarea").expect("valid");
    role(form) == "search"
        // The form names itself a search box (`<form id="search_form">`).
        || ["id", "name", "class"]
            .iter()
            .filter_map(|a| form.value().attr(a))
            .any(|value| any_token_in(value, SEARCH_FIELD_TOKENS))
        || form.value().attr("action").is_some_and(|a| {
            any_token_in(&a.replace(['/', '.', '?', '='], " "), SEARCH_FIELD_TOKENS)
        })
        || form.select(&fields).any(|f| search_field(&f))
}

/// What a page shows about being an application step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A form with at least three distinct fields to fill (radio and
    /// checkbox groups count once), not only checkboxes, no password, not a
    /// search box, and not under a search or listing title.
    ApplicationForm,
    /// A sign-in form: a password field, or a title asking to log in first.
    LoginForm,
    /// Neither.
    Other,
}

/// Distinct fillable fields in one form.
/// Distinct fillable fields in one form, whether any is more than a
/// checkbox, and whether it asks for a password.
fn fillable_fields(form: &ElementRef<'_>) -> (usize, bool, bool) {
    let fields = Selector::parse("input, select, textarea").expect("valid");
    let mut names: Vec<String> = vec![];
    let mut beyond_checkboxes = false;
    let mut password = false;
    for (i, field) in form.select(&fields).enumerate() {
        let v = field.value();
        let kind = v.attr("type").unwrap_or("text").to_ascii_lowercase();
        if kind == "password" {
            password = true;
            continue;
        }
        if v.name() == "input"
            && matches!(
                kind.as_str(),
                "hidden" | "submit" | "button" | "image" | "reset" | "search"
            )
        {
            continue;
        }
        beyond_checkboxes |= !(v.name() == "input" && kind == "checkbox");
        let key = v
            .attr("name")
            .or(v.attr("id"))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("#{i}"));
        if !names.contains(&key) {
            names.push(key);
        }
    }
    (names.len(), beyond_checkboxes, password)
}

/// The page's step, from its forms and title.
pub fn step(html: &str) -> Step {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title").expect("valid");
    let title = doc
        .select(&title)
        .next()
        .map(|t| collapse(&t.text().collect::<String>()))
        .unwrap_or_default();
    // Under a search or listing title a form searches, it does not apply
    // (the link resolver's rule, `resolve::SEARCH_TITLE`).
    let listing = crate::link_resolver::resolve::SEARCH_TITLE.is_match(&title);
    let forms = Selector::parse("form").expect("valid");
    let mut login = false;
    for form in doc.select(&forms).take(50) {
        if search_form(&form) {
            continue;
        }
        let (count, beyond_checkboxes, password) = fillable_fields(&form);
        if password {
            login = true;
        } else if count >= 3 && beyond_checkboxes && !listing {
            // Checkboxes alone are consents (a sign-up agreement), not an
            // application: an application asks for at least one answer.
            return Step::ApplicationForm;
        }
    }
    // A sidebar login box on a notice is not a gate: a password form counts
    // only under a sign-in or identity-check title.
    if (login && crate::link_resolver::resolve::GATE_TITLE.is_match(&title))
        || crate::link_resolver::classify::title_is_login_gate(&title)
    {
        Step::LoginForm
    } else {
        Step::Other
    }
}

/// The page `<title>`, whitespace collapsed, at most 150 characters.
pub fn title(html: &str) -> String {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("title").expect("valid");
    doc.select(&selector)
        .next()
        .map(|t| collapse(&t.text().collect::<String>()))
        .unwrap_or_default()
        .chars()
        .take(150)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_links_are_dropped_and_content_buttons_win() {
        let html = r#"<html><body>
          <header><ul class="gnb"><li><a href="/apply/all">신청하기</a></li></ul></header>
          <div class="right-fix-btn"><a href="/mainB.do">신청하기</a></div>
          <nav><a href="/mainB.do">신청하기</a></nav>
          <div class="mo-tab-bar"><a href="/mainB.do">신청하기</a></div>
          <div id="contents"><form id="frm"><div class="btn-ce">
            <a class="btn-bace2" href="/loanCrdtRcvr/step1.do?key=7">신청하기</a></div></form>
            <a href="/notice/guide.do">신청방법 안내</a></div>
          <footer><a href="/minwon">민원 신청하기</a></footer></body></html>"#;
        let found = apply_elements("https://youth.seoul.go.kr/youthConts.do?key=7", html);
        let top = best(&found);
        assert_eq!(top.len(), 1);
        assert_eq!(
            top[0].url,
            "https://youth.seoul.go.kr/loanCrdtRcvr/step1.do?key=7"
        );
        assert_eq!(top[0].why, "content+form+button+apply_path");
        // The repeated fixed button stays below the bar.
        assert!(found
            .iter()
            .filter(|f| f.url.ends_with("/mainB.do"))
            .all(|f| f.score < MIN_SCORE));
    }

    #[test]
    fn names_come_from_text_alt_aria_or_title_and_targets_from_onclick() {
        let html = r#"<div class="board-view">
          <a href="javascript:;" onclick="window.open('https://apply.example.go.kr/form/3')"><img alt="온라인 신청"></a>
          <button type="button" class="btn" aria-label="신청 바로가기" onclick="location.href='/aply/5'"></button>
          <a href="javascript:goApply()">신청하기</a></div>"#;
        let found = apply_elements("https://www.example.go.kr/view.do?id=1", html);
        let urls: Vec<&str> = found.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://www.example.go.kr/aply/5",
                "https://apply.example.go.kr/form/3"
            ]
        );
        assert_eq!(found[1].label, "온라인 신청");
    }

    #[test]
    fn a_page_without_an_own_button_has_no_best() {
        let html = r#"<div id="contents"><p>방문 접수</p><a href="/x">자세히</a></div>"#;
        assert!(best(&apply_elements("https://a.go.kr/v?id=1", html)).is_empty());
        // In the content but only an information link.
        let html = r#"<div class="content"><a href="/guide">신청 안내</a></div>"#;
        assert!(apply_elements("https://a.go.kr/v?id=1", html).is_empty());
    }

    #[test]
    fn forms_are_application_login_or_search() {
        let questionnaire = r#"<title>학자금대출 신용 회복</title>
          <form name="s" action="/search.do"><input name="keyWord"></form>
          <form id="frm"><input type="hidden" name="key"><input type="radio" name="q1"><input type="radio" name="q1">
          <input type="radio" name="q2"><input type="radio" name="q3"></form>"#;
        assert_eq!(step(questionnaire), Step::ApplicationForm);
        let login = r#"<title>로그인</title><form><input name="id"><input type="password" name="pw"><input name="otp"></form>"#;
        assert_eq!(step(login), Step::LoginForm);
        let search_only = r#"<form><input name="query"><select name="kind"></select><input name="sc_keyword"></form>"#;
        assert_eq!(step(search_only), Step::Other);
        let gate = "<title>로그인 후에 신청할 수 있습니다.</title>";
        assert_eq!(step(gate), Step::LoginForm);
        // A notice with a sidebar login box is not a sign-in gate.
        let notice = r#"<title>2026 청년 월세 지원 공고</title><form><input name="id"><input type="password" name="pw"></form>"#;
        assert_eq!(step(notice), Step::Other);
        let two_fields = r#"<form><input name="a"><input name="b"></form>"#;
        assert_eq!(step(two_fields), Step::Other);
    }
}
