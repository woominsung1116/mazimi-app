//! Page facts from fetched HTML, with the audit fetcher's patterns.
use super::classify::PageFacts;
use regex::Regex;
use std::sync::LazyLock;

/// Bytes read from one page (the audit read the same amount).
pub const PAGE_BYTE_LIMIT: usize = 400_000;
const MAX_LINKS: usize = 12;

static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("valid"));
static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<input\b([^>]*)").expect("valid"));
static NON_FIELD_INPUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"type=["']?(hidden|submit|button|search)"#).expect("valid"));
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\b[^>]*href=["']([^"'#][^"']*)["'][^>]*>(.*?)</a>"#).expect("valid")
});
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));
static FORM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<form\b").expect("valid"));
static TEXTAREA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<textarea\b").expect("valid"));
static SELECT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<select\b").expect("valid"));
static LINK_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)신청|접수|지원하기|참여하기|apply").expect("valid"));

/// UTF-8, or EUC-KR (CP949) when the page or header declares it. Invalid
/// bytes are replaced: the facts are counts, a title and link texts.
pub fn decode(bytes: &[u8], content_type: &str) -> String {
    let utf8 = String::from_utf8_lossy(bytes);
    let head: String = utf8.chars().take(3000).collect::<String>().to_lowercase();
    if head.contains("charset=euc-kr") || content_type.to_lowercase().contains("euc-kr") {
        return encoding_rs::EUC_KR.decode(bytes).0.into_owned();
    }
    utf8.into_owned()
}

/// Whitespace collapsed, control characters (NUL included: jsonb rejects
/// it) dropped.
fn collapse(text: &str) -> String {
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();
    SPACE.replace_all(&text, " ").trim().to_string()
}

/// The HTML character references that occur in titles, link texts and
/// hrefs (`&amp;` between query parameters above all).
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

pub fn analyze(html: &str) -> PageFacts {
    let low = html.to_lowercase();
    let title = TITLE
        .captures(html)
        .map(|c| collapse(&unescape(&c[1])).chars().take(150).collect())
        .unwrap_or_default();
    let count = |pattern: &Regex| pattern.find_iter(&low).count() as u32;
    let inputs = INPUT
        .captures_iter(&low)
        .filter(|c| !NON_FIELD_INPUT.is_match(&c[1]))
        .count() as u32;
    let mut apply_links = Vec::new();
    for found in LINK.captures_iter(html) {
        let text = collapse(&unescape(&TAG.replace_all(&found[2], "")));
        if LINK_TEXT.is_match(&text) && text.chars().count() < 40 {
            apply_links.push((
                text,
                unescape(&found[1])
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(300)
                    .collect(),
            ));
            if apply_links.len() == MAX_LINKS {
                break;
            }
        }
    }
    PageFacts {
        title,
        forms: count(&FORM),
        inputs,
        textareas: count(&TEXTAREA),
        selects: count(&SELECT),
        password: low.contains("type=\"password\"") || low.contains("type='password'"),
        apply_links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_match_the_audit_fetcher() {
        let html = r##"<html><head><title>
            2026 청년 월세 지원 신청</title></head><body>
            <form action="/x"><input type="text" name="a"><input type="hidden" name="t">
            <input name="b"><textarea></textarea><select><option>1</option></select></form>
            <a href="/apply.do?id=7"><span>온라인 신청하기</span></a>
            <a href="#top">신청</a>
            <a href='/notice'>공지사항</a>
            <a href="/apply.do?a=1&amp;b=2">접수&nbsp;하기</a>
            </body></html>"##;
        let facts = analyze(html);
        assert_eq!(facts.title, "2026 청년 월세 지원 신청");
        assert_eq!(
            (facts.forms, facts.inputs, facts.textareas, facts.selects),
            (1, 2, 1, 1)
        );
        assert!(!facts.password);
        assert_eq!(
            facts.apply_links,
            vec![
                ("온라인 신청하기".to_string(), "/apply.do?id=7".to_string()),
                ("접수 하기".to_string(), "/apply.do?a=1&b=2".to_string())
            ]
        );
    }

    #[test]
    fn control_characters_never_reach_the_stored_facts() {
        let facts = analyze("<title>a\u{0}b</title><a href=\"/x\u{0}y\">신청\u{1}하기</a>");
        assert_eq!(facts.title, "ab");
        assert_eq!(
            facts.apply_links,
            vec![("신청하기".to_string(), "/xy".to_string())]
        );
    }

    #[test]
    fn euc_kr_pages_are_decoded() {
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("<meta charset=euc-kr><title>신청</title>");
        assert_eq!(analyze(&decode(&bytes, "text/html")).title, "신청");
    }
}
