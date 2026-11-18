use mazimi_core::{application_resources::public_url, models::Program};
use reqwest::Url;
use scraper::{Html, Selector};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub kind: &'static str,
    pub url: String,
    pub label: String,
}

pub fn seeds(p: &Program) -> Vec<Candidate> {
    let mut urls = vec![];
    if let Some(url) = &p.official_url {
        urls.push(url.as_str());
    }
    if let Some(raw) = &p.raw_payload {
        for key in ["aplyUrlAddr", "refUrlAddr1", "refUrlAddr2"] {
            if let Some(url) = raw.get(key).and_then(|v| v.as_str()) {
                urls.push(url);
            }
        }
    }
    let mut found: Vec<Candidate> = vec![];
    for url in urls.into_iter().filter(|url| public_url(url)) {
        if !found.iter().any(|r| r.url == url.trim()) {
            // A programme URL that is itself a document is a form candidate,
            // not a page to read links from (10-31 dry run: 14 of 200).
            let kind = if Url::parse(url.trim()).is_ok_and(|u| document_name(u.path())) {
                "template"
            } else {
                "notice"
            };
            found.push(Candidate {
                kind,
                url: url.trim().into(),
                label: p.title.chars().take(200).collect(),
            });
        }
    }
    found
}

/// Document extensions a form may come in. Whether the file is one is
/// decided later from its bytes ([`file_format`]) and its pages (the forms
/// pipeline), never from words in a link caption.
const DOCUMENT_EXTENSIONS: &[&str] = &[".hwp", ".hwpx", ".pdf"];

/// Does this name (a URL path, a caption, a `title` or `download`
/// attribute) end in, or carry, a document file name? A caption may carry
/// the size after the name ("신청서.hwp (55KB)").
fn document_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '/' | '?' | '&' | '=')
        })
        .any(|part| {
            DOCUMENT_EXTENSIONS
                .iter()
                .any(|ext| part.ends_with(ext) && part.len() > ext.len())
        })
}

/// Extraction is deliberately only candidate discovery. A familiar domain,
/// an /apply path or a button caption cannot verify program/intake identity.
///
/// A link is a file candidate ("template") when the publisher names a
/// document file anywhere: the URL path, the caption, or the `title` or
/// `download` attribute. No caption word decides it (10-31 dry run: the old
/// word rule found 7 of 18 notices with a form). On a programme's own page
/// (`program` given, depth 0) a link whose caption is about the programme is
/// followed as a notice: a board list leads to the programme's own post.
pub fn links(base: &str, html: &str, program: Option<&str>) -> Vec<Candidate> {
    let Ok(base) = Url::parse(base) else {
        return vec![];
    };
    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut found: Vec<Candidate> = vec![];
    for a in doc.select(&selector).take(1000) {
        let label = a
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let compact = label.split_whitespace().collect::<String>();
        let Ok(url) = base.join(a.value().attr("href").unwrap_or("")) else {
            continue;
        };
        if url == base || !public_url(url.as_str()) || found.iter().any(|c| c.url == url.as_str()) {
            continue;
        }
        // A name may also sit in the query ('down.do?fileNm=신청서.hwp').
        let query_name = url
            .query_pairs()
            .map(|(_, v)| v.into_owned())
            .find(|v| document_name(v));
        let named = [
            Some(url.path()),
            query_name.as_deref(),
            Some(label.as_str()),
            a.value().attr("title"),
            a.value().attr("download"),
        ]
        .into_iter()
        .flatten()
        .find(|name| document_name(name));
        let kind = if named.is_some() {
            "template"
        } else if [
            "신청하기",
            "온라인신청",
            "접수하기",
            "지원하기",
            "참여신청",
            "신청서작성",
        ]
        .iter()
        .any(|word| compact.contains(word))
        {
            "application"
        } else if ["모집공고", "공고문", "서식다운로드"]
            .iter()
            .any(|word| compact.contains(word))
            || program.is_some_and(|program| related(&label, program) == Some(true))
        {
            "notice"
        } else {
            continue;
        };
        // A caption that is only "다운로드" says nothing; the file's own name does.
        let label = match named {
            Some(name) if kind == "template" && !document_name(&label) && name != url.path() => {
                name.to_string()
            }
            _ => label,
        };
        found.push(Candidate {
            kind,
            url: url.to_string(),
            label: label.chars().take(200).collect(),
        });
        if found.len() >= 30 {
            break;
        }
    }
    found
}

/// Query keys that carry a session or a tracking tag, never page identity.
fn volatile_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "jsessionid"
            | "sessionid"
            | "sid"
            | "phpsessid"
            | "aspsessionid"
            | "fbclid"
            | "gclid"
            | "_"
    ) || key.starts_with("utm_")
}

/// A link target as it identifies a page: `;jsessionid=` path parameters and
/// session/tracking query keys removed, fragments kept (SPA routes).
pub(crate) fn normalize_link(value: &str) -> String {
    let value = value.trim();
    let (head, fragment) = value
        .split_once('#')
        .map_or((value, None), |(h, f)| (h, Some(f)));
    let (path, query) = head
        .split_once('?')
        .map_or((head, None), |(p, q)| (p, Some(q)));
    let path = path
        .split_once(';')
        .filter(|(_, param)| param.to_ascii_lowercase().starts_with("jsessionid"))
        .map_or(path, |(p, _)| p);
    let mut out = path.to_string();
    let kept: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !volatile_query_key(pair.split('=').next().unwrap_or("")))
        .collect();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Change-detection key for a notice/application page. Only the primary
/// content counts: navigation and banners outside it are ignored, links lose
/// session/tracking parameters and labelled counters, dates and relative
/// times are replaced by placeholders (shared with the benefit-page
/// collector). A cookie-less client that gets a fresh `;jsessionid=` on every
/// request, or a board view counter, must not look like a new intake.
pub fn semantic_hash(html: &str) -> String {
    let doc = Html::parse_document(html);
    let primary = Selector::parse("main, article, .ntt_cn_container, #content, #contents").unwrap();
    let fallback = Selector::parse("body").unwrap();
    let Some(element) = doc
        .select(&primary)
        .next()
        .or_else(|| doc.select(&fallback).next())
    else {
        return mazimi_core::application_resources::digest(b"");
    };
    let text = element.text().collect::<Vec<_>>().join(" ");
    let text = crate::sources::benefit_pages::normalize_volatile_tokens(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
    );
    // Navigation targets and form semantics inside the primary content, not
    // CSS coordinates or site-wide menus.
    let form =
        Selector::parse("a[href],form[action],input[name],select[name],textarea[name]").unwrap();
    let fields: Vec<_> = element
        .select(&form)
        .take(2000)
        .filter(|e| e.value().attr("type") != Some("hidden"))
        .map(|e| {
            format!(
                "{}:{}:{}:{}",
                e.value().name(),
                normalize_link(
                    e.value()
                        .attr("href")
                        .or(e.value().attr("action"))
                        .unwrap_or("")
                ),
                e.value().attr("name").unwrap_or(""),
                e.value().attr("type").unwrap_or("")
            )
        })
        .collect();
    mazimi_core::application_resources::digest(format!("{text}\n{}", fields.join("\n")).as_bytes())
}

/// `filename*=UTF-8''x.hwp` or `filename="x.hwp"`, lower-cased. Percent
/// encoding does not touch the ASCII extension, so no decoding is needed.
fn disposition_filename(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let start = lower
        .find("filename*=")
        .map(|i| i + "filename*=".len())
        .or_else(|| lower.find("filename=").map(|i| i + "filename=".len()))?;
    let raw = lower[start..].split(';').next()?.trim().trim_matches('"');
    let name = raw.rsplit("''").next().unwrap_or(raw).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Magic bytes are mandatory. The extension must agree in at least one name
/// the publisher gave the file: URL path, Content-Disposition filename or the
/// link label. Board downloads ("/download.do?no=3", label "신청서 다운로드")
/// usually carry the name only in Content-Disposition.
pub fn file_format(
    bytes: &[u8],
    url_path: &str,
    content_disposition: Option<&str>,
    label: &str,
) -> Option<&'static str> {
    let names = [
        Some(url_path.to_lowercase()),
        content_disposition.and_then(disposition_filename),
        Some(label.trim().to_lowercase()),
    ];
    let named = |ext: &str| names.iter().flatten().any(|name| name.ends_with(ext));
    if bytes.starts_with(b"%PDF-") && named(".pdf") {
        Some("pdf")
    } else if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])
        && bytes
            .windows(b"HWP Document File".len())
            .any(|w| w == b"HWP Document File")
        && named(".hwp")
    {
        Some("hwp")
    }
    // HWPX (KS X 6101 / OWPML) starts like ODF/EPUB: the first local entry
    // is an uncompressed `mimetype` whose bytes are the media type. Only that
    // fixed header is read; the archive is never unzipped or executed. Any
    // other ZIP (DOCX, a renamed archive) stays unsupported.
    else if hwpx_mimetype(bytes) && named(".hwpx") {
        Some("hwpx")
    } else {
        None
    }
}

/// The first ZIP local file header names `mimetype`, is stored (method 0)
/// and its data starts with `application/hwp+zip`. Offsets per APPNOTE
/// 4.3.7: method at 8, name length at 26, extra length at 28, name at 30.
fn hwpx_mimetype(bytes: &[u8]) -> bool {
    const MEDIA: &[u8] = b"application/hwp+zip";
    if bytes.len() < 30 || !bytes.starts_with(b"PK\x03\x04") {
        return false;
    }
    let u16_at = |i: usize| usize::from(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
    let (method, name_len, extra_len) = (u16_at(8), u16_at(26), u16_at(28));
    let data = 30 + name_len + extra_len;
    method == 0
        && bytes.get(30..30 + name_len) == Some(b"mimetype".as_slice())
        && bytes.get(data..data + MEDIA.len()) == Some(MEDIA)
}

/// Words that say nothing about which programme a page or form is for
/// (`scripts/forms/auto/match.py` `GENERIC`, the twin of this rule).
pub(super) const GENERIC_WORDS: &[&str] = &[
    "청년",
    "지원",
    "사업",
    "신청",
    "신청서",
    "서식",
    "양식",
    "모집",
    "공고",
    "안내",
    "참여",
    "운영",
    "센터",
    "시",
    "군",
    "구",
    "도",
    "년",
    "및",
    "의",
    "등",
    "제출",
    "서류",
    "붙임",
    "별지",
    "별첨",
    "지원사업",
    "지원신청서",
];
/// Link-caption and board words that name no programme. Only the page side
/// of [`related`] drops them; a form heading in `match.py` never carries them.
const CAPTION_WORDS: &[&str] = &[
    "모집공고",
    "공고문",
    "공고",
    "다운로드",
    "첨부파일",
    "첨부",
    "바로가기",
    "자세히",
    "보기",
    "상세",
    "게시물",
    "공지사항",
    "공지",
    "알림",
    "새소식",
    "게시판",
    "목록",
    "이전글",
    "다음글",
    "new",
    "NEW",
    // Umbrella and combined notices name a period or a bundle, not a
    // programme ("2026년 상반기 청년정책 통합 공고").
    "통합",
    "상반기",
    "하반기",
    "종합",
    "연간",
    "정책",
];
const GENERIC_BIGRAMS: &[&str] = &["청년", "지원", "사업", "신청", "청서", "원사", "년지"];
pub(super) const WORD_SUFFIXES: &[&str] = &[
    "지원",
    "사업",
    "신청서",
    "신청",
    "서식",
    "대여",
    "비",
    "료",
    "금",
];

/// Runs of Hangul syllables or ASCII letters, as `[A-Za-z]{2,}|[가-힣]{2,}`.
pub(super) fn tokens(text: &str) -> Vec<String> {
    let class = |c: char| {
        if ('가'..='힣').contains(&c) {
            1
        } else if c.is_ascii_alphabetic() {
            2
        } else {
            0
        }
    };
    let mut out: Vec<String> = vec![];
    let mut current = String::new();
    let mut last = 0;
    for c in text.chars() {
        let k = class(c);
        if k != last && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if k != 0 {
            current.push(c);
        }
        last = k;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out.retain(|t| t.chars().count() >= 2);
    out
}

/// Two-syllable district names; longer place names are caught by their
/// 시/군/구 ending, which on two syllables would also drop words like 한시,
/// 연구 or 가구.
pub(super) const SHORT_DISTRICTS: &[&str] = &["중구", "동구", "서구", "남구", "북구"];

/// Distinctive words: one generic suffix and a leading 청년 stripped
/// ("청년월세" names the same thing as "청년 월세"), generic words and place
/// or institution names (…시/군/구(청) of 3+ syllables, short district
/// names, …센터, …재단) dropped.
pub(super) fn distinctive_words(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for token in tokens(text) {
        let mut t = token.clone();
        if let Some(suffix) = WORD_SUFFIXES.iter().find(|s| token.ends_with(**s)) {
            let stripped = &token[..token.len() - suffix.len()];
            if !stripped.is_empty() {
                t = stripped.to_string();
            }
        }
        if let Some(rest) = t.strip_prefix("청년") {
            if rest.chars().count() >= 2 {
                t = rest.to_string();
            }
        }
        let syllables = t.chars().count();
        let place = (syllables >= 3
            && ["시", "군", "구", "시청", "군청", "구청"]
                .iter()
                .any(|s| t.ends_with(s)))
            || ["센터", "재단"].iter().any(|s| t.ends_with(s))
            || SHORT_DISTRICTS.contains(&t.as_str());
        if syllables >= 2 && !GENERIC_WORDS.contains(&t.as_str()) && !place {
            out.insert(t);
        }
    }
    out
}

pub(super) fn bigrams(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text
        .chars()
        .filter(|c| ('가'..='힣').contains(c) || c.is_ascii_alphabetic())
        .collect();
    chars
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .filter(|b| !GENERIC_BIGRAMS.contains(&b.as_str()))
        .collect()
}

/// Is a page (its link caption or title) about this programme? The rule
/// of `scripts/forms/auto/match.py`: one shared distinctive word, or three
/// shared 2-grams (spacing differs between a board title and a programme
/// record). `None` when the page text has no distinctive word at all once
/// caption words are dropped (a link "모집공고"): it says nothing, so the
/// caller must decide.
pub fn related(page: &str, program: &str) -> Option<bool> {
    let mut words = distinctive_words(page);
    words.retain(|w| !CAPTION_WORDS.contains(&w.as_str()));
    if words.is_empty() {
        return None;
    }
    // One word containing the other counts: "면접정장" names "정장".
    let program_words = distinctive_words(program);
    let common = words.iter().any(|w| {
        program_words
            .iter()
            .any(|p| w.contains(p.as_str()) || p.contains(w.as_str()))
    });
    Some(common || bigrams(page).intersection(&bigrams(program)).count() >= 3)
}

/// Recruitment and HR documents that board-wide notices attach (`choose.py`
/// `NOT_AN_APPLICATION`): never a benefit application form.
pub fn recruitment_document(label: &str) -> bool {
    let compact: String = label.split_whitespace().collect();
    [
        "신원진술서",
        "채용신체검사",
        "신체검사서",
        "응시원서",
        "수요조사",
    ]
    .iter()
    .any(|word| compact.contains(word))
}

/// A programme whose own application is a recruitment form (internships,
/// public jobs): 응시원서 or 신체검사서 is then its real form.
pub fn job_programme(title: &str) -> bool {
    let compact: String = title.split_whitespace().collect();
    ["채용", "인턴", "일자리", "공무원", "공공근로", "일경험"]
        .iter()
        .any(|word| compact.contains(word))
}

/// May a file with this label be a template of this programme? Recruitment
/// documents only when the programme itself recruits (review M4).
pub fn template_label_allowed(label: &str, program: &str) -> bool {
    !recruitment_document(label) || job_programme(program)
}

/// The page `<title>`, whitespace collapsed; empty when absent.
pub fn page_title(html: &str) -> String {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("title").unwrap();
    doc.select(&selector)
        .next()
        .map(|t| t.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// May a file linked from this page be a template candidate? The
/// programme's own page (depth 0: its official or source URL) always may.
/// A page reached through a link must be about the programme: its link
/// caption or its `<title>` must be related to the programme name.
/// Board posts about other notices (recruitment exams, last year's round)
/// are how foreign forms reached unrelated programmes (10-16: 30 of 39).
pub fn page_may_offer_templates(depth: i32, caption: &str, title: &str, program: &str) -> bool {
    // Either text may show it: a combined notice names the programme only in
    // its title or only in its caption (review 3).
    depth == 0 || related(caption, program) == Some(true) || related(title, program) == Some(true)
}

/// Words of a notice that bundles many programmes ("2026년 상반기 청년
/// 지원사업 통합 모집공고"). Its caption names none of them, so it can never
/// be "related", yet its files belong to the programmes it lists.
const UMBRELLA_WORDS: &[&str] = &["통합", "상반기", "하반기", "종합", "연간"];
/// An umbrella notice is about support programmes: compound nouns only, so
/// 지원자 (applicants) or 사업장 (workplaces) never count.
const SUPPORT_NOUNS: &[&str] = &["지원사업", "지원금", "참여자모집", "청년정책"];
/// Recruitment, exam and appointment notices are never umbrella notices of
/// benefits, whatever period they name ("하반기 지방공무원 임용시험"), and
/// neither are workplace inspections or business plans.
const RECRUITMENT_WORDS: &[&str] = &[
    "채용",
    "임용",
    "시험",
    "합격",
    "응시",
    "공무원",
    "기간제",
    "근로자모집",
    "직원",
    "강사",
    "위원",
    "위촉",
    "선발",
    "서포터즈",
    "지원자",
    "사업장",
    "사업계획",
];
/// What an umbrella notice announces right after its umbrella word
/// ("통합공고", "통합 모집공고").
const NOTICE_HEADS: &[&str] = &["공고", "모집", "안내"];

/// An umbrella notice ("2026년 상반기 청년 지원사업 통합 모집공고"):
/// - an umbrella word as a whole token, or a token that is an umbrella word
///   followed by 공고/모집/안내 ("통합공고"); never inside another word
///   ("최종합격자", "종합사회복지관");
/// - evidence that it is about support programmes: a compound support noun
///   (지원사업, 지원금, 참여자 모집, 청년정책), or the umbrella word
///   directly followed by the notice itself ("통합 모집공고");
/// - no recruitment, exam, appointment or workplace word.
pub fn umbrella_caption(caption: &str) -> bool {
    let compact: String = caption.split_whitespace().collect();
    if RECRUITMENT_WORDS.iter().any(|word| compact.contains(word)) {
        return false;
    }
    let tokens: Vec<&str> = caption
        .split(|c: char| !(('가'..='힣').contains(&c) || c.is_ascii_alphanumeric()))
        .map(|t| t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '년'))
        .filter(|t| !t.is_empty())
        .collect();
    let heads = |rest: &str| NOTICE_HEADS.iter().any(|h| rest.starts_with(h));
    let mut umbrella = false;
    let mut announces = false;
    for (i, token) in tokens.iter().enumerate() {
        for word in UMBRELLA_WORDS {
            if *token == *word {
                umbrella = true;
                announces |= tokens.get(i + 1).is_some_and(|next| heads(next));
            } else if let Some(rest) = token.strip_prefix(word) {
                if heads(rest) {
                    umbrella = true;
                    announces = true;
                }
            }
        }
    }
    umbrella && (announces || SUPPORT_NOUNS.iter().any(|noun| compact.contains(noun)))
}

/// May this linked file become a template candidate of the programme?
/// Never a recruitment form of a programme that does not recruit. Beyond
/// that, one piece of evidence ties the file to the programme: the page is
/// the programme's own (depth 0), its caption or `<title>` is related, the
/// caption announces an umbrella notice, or the form's own title is related
/// ("청년 마음건강 지원사업 신청서" on a page captioned "심리상담 바우처").
/// The word rule itself is unchanged; only the evidence it is applied to
/// widens.
pub fn template_candidate_allowed(
    depth: i32,
    caption: &str,
    title: &str,
    form_label: &str,
    program: &str,
) -> bool {
    template_label_allowed(form_label, program)
        && (page_may_offer_templates(depth, caption, title, program)
            || umbrella_caption(caption)
            || related(form_label, program) == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_relative_templates_and_cross_domain_application_but_not_unsafe_links() {
        let html = r#"<a href="https://naver.me/abc">프로그램 신청하기</a><a href="/download?no=3">신청서 및 제출서류（서식）.hwp</a><a href="javascript:submit()">신청하기</a><a href="http://127.0.0.1/a.pdf">신청서.pdf</a><a href="/login">로그인</a>"#;
        let found = links("https://www.wonju.go.kr/notice?id=5", html, None);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, "application");
        assert_eq!(found[1].url, "https://www.wonju.go.kr/download?no=3");
    }

    /// 10-31 dry run: the caption word rule found forms on 7 of 18 notices.
    /// A file is a candidate when the publisher names a document anywhere;
    /// its bytes and pages decide later whether it is a form.
    #[test]
    fn file_links_are_candidates_by_their_file_name_not_by_caption_words() {
        let html = r#"
            <a href="/files/2026_youth.hwpx">붙임 1</a>
            <a href="/board/down.do?id=7">이력서.hwp (55KB)</a>
            <a href="/board/down.do?id=8" title="개인정보 이용 동의.pdf">다운로드</a>
            <a href="/board/down.do?id=9" download="참여확인서.HWP">받기</a>
            <a href="/board/view.do?id=10">자세히 보기</a>
            <a href="/files/photo.jpg">사진.jpg</a>
            <a href="/board/down.do?id=11">hwp 뷰어 설치</a>
            <a href="/board/down.do?fileNm=%EC%8B%A0%EC%B2%AD%EC%84%9C.hwp">받기</a>"#;
        let found = links("https://www.iksan.go.kr/board/view.do?id=1", html, None);
        let files: Vec<_> = found
            .iter()
            .filter(|c| c.kind == "template")
            .map(|c| (c.url.as_str(), c.label.as_str()))
            .collect();
        assert_eq!(
            files,
            vec![
                ("https://www.iksan.go.kr/files/2026_youth.hwpx", "붙임 1"),
                (
                    "https://www.iksan.go.kr/board/down.do?id=7",
                    "이력서.hwp (55KB)"
                ),
                (
                    "https://www.iksan.go.kr/board/down.do?id=8",
                    "개인정보 이용 동의.pdf"
                ),
                (
                    "https://www.iksan.go.kr/board/down.do?id=9",
                    "참여확인서.HWP"
                ),
                // Review: a name only in the query string.
                (
                    "https://www.iksan.go.kr/board/down.do?fileNm=%EC%8B%A0%EC%B2%AD%EC%84%9C.hwp",
                    "신청서.hwp"
                ),
            ]
        );
        // The old caption-word rule would have kept none of the first three.
        assert!(found.iter().all(|c| c.kind == "template"));
    }

    /// A programme URL that points at a board list (youthforest.iksan: 31 of
    /// 200 notices) reaches the programme's post one level down; posts about
    /// other programmes are not followed.
    #[test]
    fn a_board_list_on_the_programmes_own_page_is_followed_to_its_post() {
        let html = r#"
            <a href="/board/view.do?id=21">2026년 청년 면접정장 무료대여 참여자 모집</a>
            <a href="/board/view.do?id=22">2026년 제5회 임기제공무원 채용시험 공고</a>
            <a href="/board/list.do?page=2">다음</a>"#;
        let program = "청년 면접 정장 지원";
        let found = links(
            "https://youthforest.iksan.go.kr/board/list.do",
            html,
            Some(program),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].kind, found[0].url.as_str()),
            (
                "notice",
                "https://youthforest.iksan.go.kr/board/view.do?id=21"
            )
        );
        // Below the programme's own page (no programme given), nothing is followed by relation.
        assert!(links("https://youthforest.iksan.go.kr/board/list.do", html, None).is_empty());
    }

    #[test]
    fn a_programme_url_that_is_a_document_is_seeded_as_a_form_candidate() {
        let p: Program = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001", "program_type": "policy", "source_type": "youth_center",
            "title": "청년 이사비 지원", "program_status": "active", "is_active": true,
            "created_at": "2026-10-31T00:00:00Z", "updated_at": "2026-10-31T00:00:00Z",
            "official_url": "https://www.jangheung.go.kr/upload/2026/apply_form.HWP",
            "raw_payload": {"refUrlAddr1": "https://www.jangheung.go.kr/board/view.do?id=3"},
        }))
        .unwrap();
        let kinds: Vec<_> = seeds(&p).iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec!["template", "notice"]);
    }

    #[test]
    fn related_needs_a_distinctive_word_or_three_shared_bigrams() {
        // The 10-16 noise: board posts about recruitment exams on a benefit.
        assert_eq!(
            related(
                "new 행정지원과 2026년 제5회 임기제공무원 채용시험 최종합격자 발표 및 임용후보자 등록",
                "결혼축하금 지원사업"
            ),
            Some(false)
        );
        assert_eq!(
            related(
                "2025년 공공기관 맞춤형 취업지원사업 모집공고",
                "청년 재직자 슈퍼패스 바우처"
            ),
            Some(false)
        );
        // The programme's own notice, reached through a board link.
        assert_eq!(
            related(
                "2026년 청년 재직자 슈퍼패스 바우처 사업 모집공고",
                "청년 재직자 슈퍼패스 바우처"
            ),
            Some(true)
        );
        // Spacing differs: '면접정장' vs '면접 정장' share bigrams.
        assert_eq!(
            related("청년면접정장 무료대여 안내", "청년 면접 정장 지원"),
            Some(true)
        );
        // Review H2: spacing and a leading 청년 do not make pages unrelated,
        // and two-syllable words ending in 시/구 are words, not places.
        assert_eq!(
            related("2026년 청년월세 지원사업 공고", "청년 월세 한시 특별지원"),
            Some(true)
        );
        assert_eq!(
            related("2026년 청년 월세 지원 공고", "청년월세 한시 특별지원"),
            Some(true)
        );
        assert_eq!(
            related("한시 특별 생활비 안내", "청년 한시 생활비"),
            Some(true)
        );
        assert_eq!(related("(중구) 공고", "(중구) 청년 이사비 지원사업"), None);
        assert_eq!(related("완주군청 공지사항", "결혼축하금 지원사업"), None);
        // Umbrella and combined notices name a period, not a programme: their
        // caption says nothing, so the page title decides (review 3).
        for umbrella in [
            "2026년 상반기 청년 지원사업 통합 공고",
            "2026년 하반기 청년정책 종합 모집공고",
            "2026년 연간 청년 지원사업 통합 안내",
        ] {
            assert_eq!(related(umbrella, "청년 면접 정장 지원"), None, "{umbrella}");
        }
        // Only generic words: the caption says nothing, the caller decides.
        assert_eq!(related("모집공고", "청년 면접 정장 지원"), None);
        assert_eq!(related("공고문 다운로드", "결혼축하금 지원사업"), None);
    }

    /// The second reviewer's three captions, verbatim. Each names its
    /// programme differently (umbrella notice, synonym, national scheme).
    #[test]
    fn umbrella_and_differently_named_pages_offer_templates_only_with_evidence() {
        let allowed = template_candidate_allowed;
        // 1: an umbrella notice lists many programmes; its caption names none.
        let c1 = "2026년 상반기 청년 지원사업 통합 모집공고";
        assert!(umbrella_caption(c1));
        assert!(allowed(
            1,
            c1,
            "공지사항 | 시청",
            "신청서.hwp",
            "청년 이사비 지원"
        ));
        // ...but a recruitment form on it is still not this programme's.
        assert!(!allowed(
            1,
            c1,
            "공지사항 | 시청",
            "응시원서.hwp",
            "청년 이사비 지원"
        ));
        // 2 and 3: the caption is unrelated; the page title or the form's own
        // title must name the programme, else the file is not a candidate.
        let c2 = "청년 심리상담 바우처 참여자 모집";
        let p2 = "청년 마음건강 지원사업";
        assert_eq!(related(c2, p2), Some(false));
        assert!(!umbrella_caption(c2));
        assert!(allowed(
            1,
            c2,
            "청년 마음건강 지원사업 참여자 모집 | 시청",
            "신청서.hwp",
            p2
        ));
        assert!(allowed(
            1,
            c2,
            "공지사항 | 시청",
            "청년 마음건강 지원사업 신청서.hwp",
            p2
        ));
        assert!(!allowed(1, c2, "공지사항 | 시청", "신청서.hwp", p2));
        let c3 = "국민취업지원제도 안내";
        let p3 = "청년 구직활동 지원금";
        assert_eq!(related(c3, p3), Some(false));
        assert!(allowed(
            1,
            c3,
            "청년 구직활동 지원금 안내 | 고용노동부",
            "신청서.hwp",
            p3
        ));
        assert!(allowed(
            1,
            c3,
            "공지사항",
            "청년 구직활동 지원금 신청서.hwp",
            p3
        ));
        assert!(!allowed(1, c3, "공지사항", "참여신청서.hwp", p3));
        // The matching rule itself is not loosened: recruitment forms on an
        // unrelated board post stay out.
        assert!(!allowed(
            1,
            "new 행정지원과 2026년 제5회 임기제공무원 채용시험 최종합격자 발표",
            "공지사항 | 완주군청",
            "(서식1) 신원진술서.hwp",
            "결혼축하금 지원사업"
        ));
        assert!(!allowed(
            1,
            "2027 지역대기질 개선 시범사업 공고",
            "공지사항",
            "신청서.hwp",
            "청년 면접 정장 지원"
        ));
        // Umbrella words inside other words, or on recruitment and exam
        // pages, are not umbrella notices (re-check HIGH).
        for caption in [
            "최종합격자 발표",
            "하반기 지방공무원 임용시험 시행계획 공고",
            "상반기 기간제근로자 채용 공고",
            "OO종합사회복지관 프로그램 참여자 모집",
        ] {
            assert!(!umbrella_caption(caption), "{caption}");
            assert!(
                !allowed(
                    1,
                    caption,
                    "공지사항 | 시청",
                    "신청서.hwp",
                    "청년 이사비 지원"
                ),
                "{caption}"
            );
        }
        assert!(umbrella_caption("2026년 하반기 청년 지원사업 통합 안내"));
        // The programme's own page still offers any non-recruitment file.
        assert!(allowed(0, p2, "", "서식.hwp", p2));
    }

    /// Re-check of 37eb4c43: support nouns inside other words and
    /// recruitment of instructors, committee members or supporters are not
    /// umbrella notices; compound umbrella tokens and 청년정책 are.
    #[test]
    fn umbrella_captions_need_a_support_programme_and_no_recruitment() {
        for caption in [
            "하반기 사업장 정기점검 안내",
            "연간 사업계획 공고",
            "하반기 청년인턴 지원자 명단 공고",
            "2026 하반기 체육강사 지원 안내",
            "상반기 위원 위촉 공고 (사업)",
            "상반기 청년 서포터즈 참여자 모집",
        ] {
            assert!(!umbrella_caption(caption), "{caption}");
        }
        for caption in [
            "2026년 하반기 청년정책 종합 모집공고",
            "2026년 청년 지원사업 통합공고",
            "통합 모집공고(청년 월세·이사비)",
            "2026년 상반기 청년 지원사업 통합 모집공고",
        ] {
            assert!(umbrella_caption(caption), "{caption}");
        }
        // Every umbrella word works as a whole token.
        for word in UMBRELLA_WORDS {
            let caption = format!("2026년 {word} 청년 지원사업 공고");
            assert!(umbrella_caption(&caption), "{caption}");
            // ...and never inside another word.
            let inside = format!("가{word}나 지원사업");
            assert!(!umbrella_caption(&inside), "{inside}");
        }
    }

    #[test]
    fn recruitment_and_hr_documents_are_never_templates() {
        for label in [
            "(서식1) 신원진술서 제20호서식(동의서포함).hwp(55KB)",
            "(서식2) 신체검사용 채용신체검사서(병원양식으로 대체가능).hwp",
            "응시원서 등 제출서류(제5회 및 제6회).hwp",
            "(서식) 수요조사 신청서.hwpx",
        ] {
            assert!(recruitment_document(label), "{label}");
            assert!(
                !template_label_allowed(label, "결혼축하금 지원사업"),
                "{label}"
            );
        }
        // A programme that recruits uses these as its own application.
        assert!(template_label_allowed(
            "응시원서.hwp",
            "2026년 청년 인턴 채용 공고"
        ));
        assert!(template_label_allowed("신체검사서.hwp", "공공근로사업"));
        assert!(!recruitment_document(
            "2026년 청년 면접 정장 지원 신청서.hwp"
        ));
        // An employment subsidy form is a benefit form, not a recruitment one.
        assert!(!recruitment_document("청년 채용 장려금 지원 신청서.hwp"));
    }

    #[test]
    fn hwpx_is_detected_from_its_stored_mimetype_entry() {
        // Local file header: method 0 (stored), name 'mimetype', no extra field.
        let mut zip = b"PK\x03\x04\x0a\x00\x00\x00\x00\x00".to_vec();
        zip.extend_from_slice(&[0; 16]); // time, date, crc, sizes
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(b"mimetypeapplication/hwp+zipPK\x03\x04");
        assert_eq!(zip[30..38], *b"mimetype");
        assert_eq!(file_format(&zip, "/a.hwpx", None, "신청서"), Some("hwpx"));
        assert_eq!(
            file_format(
                &zip,
                "/download.do",
                Some("attachment; filename=\"x.HWPX\""),
                "다운로드"
            ),
            Some("hwpx")
        );
        // Named .hwp but a zip: the name must agree with the bytes.
        assert_eq!(file_format(&zip, "/a.hwp", None, "a.hwp"), None);
        // A deflated mimetype entry, another mimetype (OOXML/ODF) or a bare
        // zip signature is not HWPX.
        let mut deflated = zip.clone();
        deflated[8] = 8;
        assert_eq!(file_format(&deflated, "/a.hwpx", None, "a.hwpx"), None);
        let odf =
            String::from_utf8_lossy(&zip).replace("application/hwp+zip", "application/vnd.oasis");
        assert_eq!(file_format(odf.as_bytes(), "/a.hwpx", None, "a.hwpx"), None);
        assert_eq!(file_format(b"PK\x03\x04", "/a.hwpx", None, "a.hwpx"), None);
        // An extra field shifts the content; it is honoured, not guessed.
        let mut extra = zip[..30].to_vec();
        extra[28] = 4;
        extra.extend_from_slice(b"mimetype\0\0\0\0application/hwp+zip");
        assert_eq!(file_format(&extra, "/a.hwpx", None, "a.hwpx"), Some("hwpx"));
    }
    const HWP: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1....HWP Document File";

    #[test]
    fn format_and_semantics_are_not_extension_or_css_guesses() {
        assert_eq!(
            file_format(b"<html>Login</html>", "/a.pdf", None, "신청서.pdf"),
            None
        );
        assert_eq!(
            file_format(b"%PDF-1.7", "/x", None, "신청서.pdf"),
            Some("pdf")
        );
        assert_eq!(
            semantic_hash("<main class='old'>지원 조건</main>"),
            semantic_hash("<main class='new'>지원 조건</main>")
        );
        assert_ne!(
            semantic_hash("<main>2026년</main>"),
            semantic_hash("<main>2025년</main>")
        );
    }

    #[test]
    fn file_format_accepts_any_publisher_name_but_never_skips_magic_bytes() {
        // Discovered via URL path; label is a caption.
        assert_eq!(
            file_format(HWP, "/files/2026_apply.hwp", None, "신청서 다운로드"),
            Some("hwp")
        );
        // Board download: name only in Content-Disposition (RFC 5987 form).
        assert_eq!(
            file_format(
                HWP,
                "/download.do",
                Some("attachment; filename*=UTF-8''%EC%8B%A0%EC%B2%AD%EC%84%9C.HWP"),
                "다운로드"
            ),
            Some("hwp")
        );
        assert_eq!(
            file_format(
                b"%PDF-1.4",
                "/download.do",
                Some("attachment; filename=\"form.pdf\""),
                "다운로드"
            ),
            Some("pdf")
        );
        // No publisher name at all: not accepted on magic bytes alone.
        assert_eq!(
            file_format(b"%PDF-1.4", "/download.do", None, "다운로드"),
            None
        );
        // A name that disagrees with the bytes is rejected.
        assert_eq!(file_format(HWP, "/a.pdf", None, "a.pdf"), None);
    }

    #[test]
    fn semantic_hash_ignores_sessions_tracking_counters_and_navigation() {
        let page = |nav: &str, link: &str, views: &str| {
            format!("<nav><a href='/menu?{nav}'>메뉴</a></nav><main>2026년 청년 월세 지원 모집 공고 조회수 {views} <a href='{link}'>신청하기</a></main>")
        };
        let base = semantic_hash(&page(
            "a=1",
            "/apply.do;jsessionid=ABC123?id=7&utm_source=x",
            "12",
        ));
        assert_eq!(
            base,
            semantic_hash(&page(
                "a=2",
                "/apply.do;jsessionid=ZZZ999?id=7&utm_source=y",
                "13"
            ))
        );
        assert_eq!(base, semantic_hash(&page("a=1", "/apply.do?id=7", "1,204")));
        // A different destination or intake text is a real change.
        assert_ne!(base, semantic_hash(&page("a=1", "/apply.do?id=8", "12")));
        assert_ne!(
            base,
            semantic_hash(&page("a=1", "/apply.do?id=7", "12").replace("2026년", "2027년"))
        );
        // Hidden CSRF/session inputs do not count; named fields do.
        let form = |hidden: &str| {
            format!("<main><form action='/apply'><input type='hidden' name='csrf' value='{hidden}'><input name='phone'></form></main>")
        };
        assert_eq!(semantic_hash(&form("a")), semantic_hash(&form("b")));
    }
}
