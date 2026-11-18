//! One-time bootstrap of corporate offers from discovered candidate pages
//! (design §2.2 policy, §3.4 deterministic pass B, §3.5 safeguards).
//!
//! Input: the candidate export (`benefit_source_candidates` in `review`
//! with their verified domain and brand). For every candidate URL:
//!
//! 1. **Policy, before any request.** Campus domains (no brand) are out of
//!    scope; domains a person already rejected, or whose earlier review
//!    found commercial-reuse terms, are skipped; the URL must sit on the
//!    brand's verified domain; archive/aggregator hosts and third-party
//!    content paths (user repositories) are rejected (R1/R2).
//! 2. **Polite fetch** through [`DomainSession`] and [`PinnedHttp`]: robots
//!    for `MazimiBenefitBot` on the exact path and query, a 10 s floor (or
//!    the Crawl-delay), one request in flight per domain, a stop on
//!    401/403/429/challenge, at most one same-host redirect. The domain's
//!    terms page is classified after the first page; a terms clause that
//!    names collection or republishing for third parties stops the domain,
//!    a generic automated-access clause proceeds (G1).
//! 3. **Deterministic offer reading.** Visible text only, split at block
//!    elements. A page qualifies only when a line names a student/youth
//!    audience as the target of a benefit *and* carries a numeric or
//!    explicit fact (percent, KRW, free period, free plan, age band); the
//!    page may describe at most one offer and must not be past its end date.
//!    Every fact keeps a verbatim quote of at most 100 characters.
//!
//! The output is a proposal list for an independent reviewer. No page body
//! is stored and nothing is written to the database.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use regex::Regex;
use reqwest::Url;
use scraper::{node::Node, Html};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::LazyLock,
};

use super::census::OptOutCheck;
use super::evidence::{read_quote, visible_text, PageEvidence};
use super::http::PageHttp;
use super::policy::{
    self, classify_tos, find_terms_link, DomainSession, Fetched, PageState, TosSignal,
    JS_SHELL_CHARS,
};
use crate::ai_extraction::crosscheck::parse_age;

/// Longest evidence quote per fact (design §2.1, 저작권법 row).
pub const MAX_QUOTE_CHARS: usize = 100;
/// Requests per domain in one bootstrap run (A4: ≤ 200 pages a day).
pub const MAX_REQUESTS_PER_DOMAIN: u32 = 200;

// ── input ────────────────────────────────────────────────────────────────

/// One row of the candidate export. Unknown fields are ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CandidateRow {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub domain_id: Option<String>,
    pub domain: String,
    #[serde(default)]
    pub organization_name: Option<String>,
    #[serde(default)]
    pub brand_id: Option<String>,
    #[serde(default)]
    pub brand_name: Option<String>,
    #[serde(default)]
    pub compliance_status: Option<String>,
    #[serde(default)]
    pub policy_notes: Option<String>,
    #[serde(default)]
    pub allow_subdomains: Option<bool>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub domain_active: Option<bool>,
    #[serde(default)]
    pub page_title: Option<String>,
    /// A campus programme is served from this source (set by callers that
    /// know the programmes, e.g. fresh extraction).
    #[serde(default)]
    pub campus_programme: Option<bool>,
    /// The domain is on the `universities` list (set by callers with DB access).
    #[serde(default)]
    pub university_domain: Option<bool>,
    /// What the page is read for: a student or youth offer (default), or
    /// any one discount of a door's brand ([`Scope::Brand`]). Stored with
    /// the door and passed by every reader of the candidate (review H5).
    #[serde(default)]
    pub scope: Scope,
}

/// What a page is read for (review H5).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// A benefit whose target is students or young people (rule 1).
    #[default]
    Audience,
    /// One discount of the door's brand, on a page a door produced: the
    /// audience gate is replaced by a discount cue in the fact's own
    /// sentence; every other guard of [`extract_offer`] still applies.
    Brand(BrandScope),
}

/// The door's brand and the issuers a fact may name as its sponsor.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BrandScope {
    pub brand_id: uuid::Uuid,
    pub brand_name: String,
    pub door_id: uuid::Uuid,
    #[serde(default)]
    pub issuers: Vec<ScopeIssuer>,
    /// A page a list found (a child door): it must also be an article of
    /// now ([`dated_article`], review M3).
    #[serde(default)]
    pub dated: bool,
    /// Normalised names of other registry brands that contain an issuer's
    /// name ("토스뱅크", "카카오t"): the longer name wins and binds no one
    /// (code review C-2).
    #[serde(default)]
    pub covering: Vec<String>,
}

/// An issuer (a brand with a holder key) as brand scope reads it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ScopeIssuer {
    pub brand_id: uuid::Uuid,
    pub name: String,
    /// Normalised names ([`super::doors::registry::normalize`]).
    pub names: Vec<String>,
    /// `field:value` ([`mazimi_core::holder::Holder`]).
    pub holder: String,
}

impl BrandScope {
    /// The scope of `brand_id` with every issuer of `registry`.
    pub fn new(
        brand_id: uuid::Uuid,
        brand_name: &str,
        door_id: uuid::Uuid,
        registry: &super::doors::registry::Registry,
    ) -> Self {
        Self {
            dated: false,
            brand_id,
            brand_name: brand_name.to_string(),
            door_id,
            issuers: registry
                .brands()
                .iter()
                .filter_map(|b| {
                    let holder = b.holder?;
                    Some(ScopeIssuer {
                        brand_id: b.brand_id,
                        name: b.name.clone(),
                        names: b.names.clone(),
                        holder: format!("{}:{}", holder.field.name(), holder.value),
                    })
                })
                .collect(),
            covering: {
                let issuer_names: Vec<&String> = registry
                    .brands()
                    .iter()
                    .filter(|b| b.holder.is_some())
                    .flat_map(|b| &b.names)
                    .collect();
                let mut names: Vec<String> = registry
                    .brands()
                    .iter()
                    .filter(|b| b.holder.is_none())
                    .flat_map(|b| b.names.iter())
                    .filter(|n| {
                        issuer_names
                            .iter()
                            .any(|i| n.contains(i.as_str()) && n != i)
                    })
                    .cloned()
                    .collect();
                names.sort();
                names.dedup();
                names
            },
        }
    }

    fn issuer_registry(&self) -> super::doors::registry::Registry {
        let covering = super::doors::registry::Brand {
            brand_id: uuid::Uuid::nil(),
            name: String::new(),
            names: self.covering.clone(),
            must_have: false,
            holder: None,
        };
        super::doors::registry::Registry::new(
            self.issuers
                .iter()
                .map(|i| super::doors::registry::Brand {
                    brand_id: i.brand_id,
                    name: i.name.clone(),
                    names: i.names.clone(),
                    must_have: false,
                    holder: mazimi_core::holder::Holder::parse(&i.holder),
                })
                .chain(std::iter::once(covering))
                .collect(),
        )
    }
}

// ── policy before any request ────────────────────────────────────────────

/// Hosts that are never a first-party source: archives and caches (R1),
/// benefit aggregators and user-generated content (R2).
const ARCHIVE_OR_AGGREGATOR_HOSTS: &[&str] = &[
    "web.archive.org",
    "archive.org",
    "archive.ph",
    "archive.today",
    "webcache.googleusercontent.com",
    "cachedview.nl",
    "myunidays.com",
    "studentbeans.com",
    "blog.naver.com",
    "m.blog.naver.com",
    "cafe.naver.com",
    "tistory.com",
    "brunch.co.kr",
    "namu.wiki",
    "velog.io",
    "medium.com",
];

/// Hosts that serve third-party content next to the owner's own pages: only
/// the listed first-party path prefixes count as the brand's page.
const MIXED_CONTENT_HOSTS: &[(&str, &[&str])] = &[
    ("github.com", &["/education", "/features", "/pricing"]),
    ("www.youtube.com", &[]),
    ("youtube.com", &[]),
];

fn host_matches(host: &str, listed: &str) -> bool {
    host == listed || host.ends_with(&format!(".{listed}"))
}

/// A university host: Korean academic (`ac.kr`) or `.edu` domains.
pub fn campus_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    host.ends_with(".ac.kr") || host == "ac.kr" || host.ends_with(".edu")
}

/// Decides whether a candidate may be requested at all. `Err` carries the
/// rejection reason. Campus scope (no brand, a campus programme, a
/// university domain or host) is out: campus benefits need school rules.
pub fn precheck(row: &CandidateRow) -> Result<Url, &'static str> {
    let has_brand = row
        .brand_name
        .as_deref()
        .is_some_and(|b| !b.trim().is_empty());
    if !has_brand
        || row.campus_programme == Some(true)
        || row.university_domain == Some(true)
        || campus_host(&row.domain)
    {
        return Err("campus_out_of_scope");
    }
    if row.verification_status.as_deref().unwrap_or("verified") != "verified"
        || row.domain_active == Some(false)
    {
        return Err("domain_not_verified");
    }
    if row.compliance_status.as_deref() == Some("rejected") {
        return Err("domain_compliance_rejected");
    }
    let notes = row.policy_notes.as_deref().unwrap_or("").to_lowercase();
    if notes.contains("terms restrict") || notes.contains("약관") && notes.contains("제한") {
        return Err("prior_review_terms_restrict");
    }
    let Ok(url) = Url::parse(row.url.trim()) else {
        return Err("invalid_url");
    };
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return Err("not_https");
    }
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let domain = row.domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if campus_host(&host) {
        return Err("campus_out_of_scope");
    }
    if ARCHIVE_OR_AGGREGATOR_HOSTS
        .iter()
        .any(|listed| host_matches(&host, listed))
    {
        return Err("archive_or_aggregator");
    }
    let same = host == domain
        || (row.allow_subdomains == Some(true) && host.ends_with(&format!(".{domain}")));
    if !same {
        return Err("off_brand_domain");
    }
    if let Some((_, prefixes)) = MIXED_CONTENT_HOSTS.iter().find(|(h, _)| *h == host) {
        let path = url.path();
        if !prefixes
            .iter()
            .any(|p| path == *p || path.starts_with(&format!("{p}/")))
        {
            return Err("third_party_content");
        }
    }
    Ok(url)
}

// ── visible lines ────────────────────────────────────────────────────────

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "li",
    "ul",
    "ol",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "td",
    "th",
    "tr",
    "dt",
    "dd",
    "dl",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "table",
    "form",
    "blockquote",
    "figcaption",
    "caption",
    "button",
    "label",
    "option",
    "body",
];

/// Visible text split at block elements. Uses the same hidden-text rules as
/// [`visible_text`], so every line is a contiguous run of that text.
pub fn visible_lines(html: &str) -> Vec<String> {
    visible_lines_linked(html)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

/// One visible line and the byte spans of it that are link text (an inline
/// `<a>` label): a link names another page, never this page's offer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Line {
    pub text: String,
    pub links: Vec<(usize, usize)>,
    /// Bytes of `text` that are a short heading read with this line (0: none).
    pub heading: usize,
}

impl Line {
    fn in_link(&self, at: usize) -> bool {
        self.links
            .iter()
            .any(|(start, end)| (*start..*end).contains(&at))
    }

    /// The line's text with its link text cut out.
    fn without_links(&self) -> String {
        let mut out = String::new();
        let mut at = 0;
        let mut links = self.links.clone();
        links.sort();
        for (start, end) in links {
            if start >= at && end <= self.text.len() {
                out.push_str(&self.text[at..start]);
                out.push(' ');
                at = end;
            }
        }
        out.push_str(&self.text[at.min(self.text.len())..]);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// A short heading read together with the line under it.
    fn headed(heading: &Line, next: &Line) -> Line {
        let shift = heading.text.len() + 1;
        let mut links = heading.links.clone();
        links.extend(next.links.iter().map(|(s, e)| (s + shift, e + shift)));
        Line {
            text: format!("{} {}", heading.text, next.text),
            links,
            heading: heading.text.len(),
        }
    }
}

/// [`visible_lines`] with each line's link text marked.
pub fn visible_lines_linked(html: &str) -> Vec<Line> {
    let document = Html::parse_document(html);
    let mut lines: Vec<Line> = Vec::new();
    let mut current = Line::default();
    let mut current_block = None;
    'nodes: for node in document.root_element().descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let mut block = None;
        let mut linked = false;
        for ancestor in node.ancestors() {
            let Some(element) = ancestor.value().as_element() else {
                continue;
            };
            let style = element
                .attr("style")
                .unwrap_or("")
                .to_ascii_lowercase()
                .replace(' ', "");
            if matches!(
                element.name(),
                "script" | "style" | "noscript" | "template" | "svg" | "head" | "title"
            ) || element.attr("hidden").is_some()
                || element.attr("aria-hidden") == Some("true")
                || style.contains("display:none")
                || style.contains("visibility:hidden")
            {
                continue 'nodes;
            }
            // A link label is inline text; an `<a>` that wraps blocks (an
            // event card: `<a><dt>카카오 톡학생증 인증 최대 25% 할인</dt></a>`)
            // is the page's own content.
            linked |= element.name() == "a"
                && !ancestor.descendants().any(|d| {
                    d.value()
                        .as_element()
                        .is_some_and(|e| BLOCK_TAGS.contains(&e.name()))
                });
            if block.is_none() && BLOCK_TAGS.contains(&element.name()) {
                block = Some(ancestor.id());
            }
        }
        let piece = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if piece.is_empty() {
            continue;
        }
        if block != current_block && !current.text.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        current_block = block;
        if !current.text.is_empty() {
            current.text.push(' ');
        }
        let start = current.text.len();
        current.text.push_str(&piece);
        if linked {
            current.links.push((start, current.text.len()));
        }
    }
    if !current.text.is_empty() {
        lines.push(current);
    }
    lines
}

/// `<title>` text, for the reviewer only (never published).
pub fn page_title(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = scraper::Selector::parse("title").ok()?;
    let title = document
        .select(&selector)
        .next()?
        .text()
        .collect::<String>();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then(|| title.chars().take(120).collect())
}

// ── offer reading (deterministic pass B) ─────────────────────────────────

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid bootstrap pattern")
}

/// A student/youth audience named as the *target* of a benefit, in either
/// order, within a short span. "대학생을 위한 한자의 이해" (a book title)
/// does not match; "학생은 모든 앱 65% 할인" does.
static AUDIENCE_FORWARD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(대학생|대학원생|대학\(원\)생|재학생|학생|청년|청소년|교직원|교육자|교사|students?|educators?|teachers?|university|college|youth|education|교육용)[^.!?。\n()\[\]]{0,20}?(할인|전용|혜택|요금제|인증|대상|우대|특가|교육\s*가|가격|플랜|멤버십|무료|스토어|패스|요금|쿠폰|크레딧|discount|free|offer|plan|pricing|price|save|off\b|pack|license|licence)",
    )
});
/// Korean benefit-before-audience only when directly attached
/// ("할인 대상: 대학생"); a longer span crosses into unrelated titles.
static AUDIENCE_REVERSE_KO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(할인|혜택|무료|요금제)\s*(대상|대상자)?\s*[:：]?\s*(대학생|대학원생|재학생|학생|청년)")
});
static AUDIENCE_REVERSE_EN: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(discount|free|offer|plan|pricing)\b[^.!?\n()\[\]]{0,20}?\b(students?|educators?|teachers?|youth)\b|\bfor\s+(?:[a-z]+\s+){0,2}?(students?|youth)\b",
    )
});
/// "대학생을 위한 …", "대학생의 …": the audience word names a topic or a
/// title, not who receives a benefit.
static TOPIC_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(들)?(을|를)\s*위한|^(들)?의\s|^(들)?이\s*쉽게|^활"));

/// Byte span of the first audience-as-benefit-target match in `line`.
pub fn find_target(line: &str) -> Option<(usize, usize)> {
    let forward = AUDIENCE_FORWARD.captures_iter(line).find_map(|c| {
        let audience = c.get(1)?;
        (!TOPIC_SUFFIX.is_match(&line[audience.end()..])).then(|| {
            let m = c.get(0).expect("match");
            (m.start(), m.end())
        })
    });
    let reverse = AUDIENCE_REVERSE_KO
        .find(line)
        .or_else(|| AUDIENCE_REVERSE_EN.find(line))
        .map(|m| (m.start(), m.end()));
    match (forward, reverse) {
        (Some(f), Some(r)) => Some(if f.0 <= r.0 { f } else { r }),
        (f, r) => f.or(r),
    }
}
static AUDIENCE_CODES: &[(&str, &[&str])] = &[
    (
        "university_student",
        &[
            "대학생",
            "대학(원)생",
            "재학생",
            "university student",
            "college student",
            "higher education",
        ],
    ),
    ("graduate", &["대학원생", "graduate student"]),
    ("student", &["학생", "student"]),
    ("youth", &["청년", "youth"]),
    ("teen", &["청소년"]),
    (
        "educator",
        &["교직원", "교육자", "교사", "educator", "teacher", "faculty"],
    ),
];
/// Who a stated value serves: the student or youth audience it is for, a
/// teacher, or an institution (`Schools can save up to 75%`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Party {
    StudentYouth,
    Teacher,
    Institution,
}

/// Institutions as the recipient of a value: plural or with a Korean
/// particle, so `school email` or `학교 이메일` (a way to verify) is none.
const INSTITUTION_WORDS: &str = r"(?i)\b(?:schools|institutions|districts|school\s+districts)\b|학교(?:는|들|에서|에|가|와|과|및|·|,|\s*전용)|(?:교육\s*)?기관(?:은|는|이|가|들|에|과|와|및)?";

/// Every audience named in `text` ([`AUDIENCE_CODES`]: `educator` is a
/// teacher, every other code a student or youth; [`INSTITUTION_WORDS`]),
/// with its byte span, in order, longest word first where two overlap.
fn audience_mentions(text: &str) -> Vec<(usize, usize, Party)> {
    static PEOPLE: LazyLock<Regex> = LazyLock::new(|| {
        let mut words: Vec<&str> = AUDIENCE_CODES
            .iter()
            .flat_map(|(_, words)| words.iter().copied())
            .collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.len()));
        let alternation: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
        re(&format!("(?i){}", alternation.join("|")))
    });
    static INSTITUTION: LazyLock<Regex> = LazyLock::new(|| re(INSTITUTION_WORDS));
    let mut found: Vec<(usize, usize, Party)> = PEOPLE
        .find_iter(text)
        .map(|m| {
            let lower = m.as_str().to_lowercase();
            let educator = AUDIENCE_CODES
                .iter()
                .any(|(code, words)| *code == "educator" && words.contains(&lower.as_str()));
            let party = if educator {
                Party::Teacher
            } else {
                Party::StudentYouth
            };
            // The whole word: `students`, `teachers`.
            let end = m.end()
                + text[m.end()..]
                    .find(|c: char| !c.is_ascii_alphabetic())
                    .unwrap_or(text.len() - m.end());
            (m.start(), end, party)
        })
        .collect();
    for m in INSTITUTION.find_iter(text) {
        if !found.iter().any(|(s, e, _)| m.start() < *e && *s < m.end()) {
            found.push((m.start(), m.end(), Party::Institution));
        }
    }
    found.sort_by_key(|(start, _, _)| *start);
    found
}

/// Text between two audiences of one list (`students, teachers, and
/// academic institutions`, `학생과 교사`): separators, a conjunction and
/// at most one word.
static LIST_GAP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:과|와|및|이나|나)?[\s,&/·]*(?:(?:and|or|nor|및|또는)\s+)?(?:[a-z-]{1,14}\s+)?$")
});

/// Rule 1 (audience bound to the value): the audience nearest to the value
/// at `value` (before or after it; a tie goes to the one before), together
/// with the audiences listed with it, includes a student or youth audience.
/// An institution or a teacher nearer to the value voids it.
/// `mentions` are spans of `text`.
fn bound_to_students(
    text: &str,
    mentions: &[(usize, usize, Party)],
    value: (usize, usize),
) -> bool {
    let distance = |(start, end, _): &(usize, usize, Party)| {
        if *end <= value.0 {
            (value.0 - end, 0)
        } else {
            (start.saturating_sub(value.1), 1)
        }
    };
    let Some(nearest) = (0..mentions.len()).min_by_key(|&i| distance(&mentions[i])) else {
        return false;
    };
    let listed = |a: usize, b: usize| {
        let gap = text.get(mentions[a].1..mentions[b].0).unwrap_or(".");
        LIST_GAP.is_match(gap)
    };
    let mut first = nearest;
    while first > 0 && listed(first - 1, first) {
        first -= 1;
    }
    let mut last = nearest;
    while last + 1 < mentions.len() && listed(last, last + 1) {
        last += 1;
    }
    mentions[first..=last]
        .iter()
        .any(|(_, _, party)| *party == Party::StudentYouth)
}

/// Rule 2 (someone else's offer): a sentence whose subject is a
/// verification programme ([`VERIFICATION`]: the GitHub Student Developer
/// Pack, SheerID, UNiDAYS) states that programme's benefit (`The GitHub
/// Student Developer Pack gives students free access to …`), not the page
/// owner's offer. `GitHub Student Developer Pack offer: 6 months free` and
/// `Students enrolled in the Pack get …` are the owner's.
fn programme_subject(sentence: &str) -> bool {
    static SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
        let names: Vec<String> = VERIFICATION
            .iter()
            .filter(|(code, _)| PROGRAMMES.contains(code))
            .flat_map(|(_, words)| words.iter().map(|w| regex::escape(w)))
            .collect();
        re(&format!(
            r"(?i)^\W*(?:the\s+)?(?:{}|{THIRD_PARTY})[a-z ]{{0,24}}?(?:\s*\([^)]*\))?\s*(?:(?:is|was|has|gives|provides|offers|includes|grants|lets|brings|unlocks|helps|contains|features)\b|(?:은|는|이|가|에서)\s)",
            names.join("|")
        ))
    });
    SUBJECT.is_match(sentence)
}

/// Another company named as a sentence's subject (`제휴사는 …`, `제휴
///카드사에서 …`): its benefit, not the page owner's offer.
const THIRD_PARTY: &str = r"제휴\s*(?:사|처|카드사|카드|은행|업체)|파트너사|협력사";

/// The [`VERIFICATION`] codes that name a programme of their own (a school
/// email or a student ID is a way to verify, not a programme).
const PROGRAMMES: &[&str] = &["sheerid", "unidays", "student_beans", "github_student"];

/// Verifying or signing up (`인증하면`, `verify your status`, `회원 가입`):
/// a free claim in such a clause that names a verification programme is the
/// programme's step (`Student Beans에서 학생 인증은 무료입니다`), not the offer.
static VERIFY_ACT: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\bverif|\bsign(?:ing)?\s*up\b|\bregist|인증|가입|등록"));

/// Whether `clause` is the verification programme's own step.
fn verification_step(clause: &str) -> bool {
    static NAMES: LazyLock<Regex> = LazyLock::new(|| {
        let names: Vec<String> = VERIFICATION
            .iter()
            .filter(|(code, _)| PROGRAMMES.contains(code))
            .flat_map(|(_, words)| words.iter().map(|w| regex::escape(w)))
            .collect();
        re(&format!("(?i){}", names.join("|")))
    });
    VERIFY_ACT.is_match(clause) && NAMES.is_match(clause)
}

/// [`programme_subject`] of a sentence of `line`, read without the heading
/// it may open with (`제휴 혜택` + `제휴사는 …`).
fn someone_elses(line: &Line, (start, end): (usize, usize)) -> bool {
    let sentence = &line.text[start..end];
    if programme_subject(sentence.trim_start()) {
        return true;
    }
    start < line.heading
        && line
            .text
            .get(line.heading..end)
            .is_some_and(|body| programme_subject(body.trim_start()))
}

/// Sentences of `text` as byte spans: a `.`, `!`, `?` or `。` followed by
/// whitespace or the end closes one.
fn sentence_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, n)| *n);
        if matches!(c, '.' | '!' | '?' | '。' | '？') && next.is_none_or(char::is_whitespace) {
            let end = at + c.len_utf8();
            if !text[start..end].trim().is_empty() {
                out.push((start, end));
            }
            start = end;
        }
    }
    if !text[start..].trim().is_empty() {
        out.push((start, text.len()));
    }
    // A piece with no word (a footnote mark `1)`, `*`) belongs to the
    // sentence before it.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in out {
        let wordless = !text[start..end].chars().any(char::is_alphabetic);
        match merged.last_mut() {
            Some(last) if wordless => last.1 = end,
            _ => merged.push((start, end)),
        }
    }
    merged
}

static KRW: LazyLock<Regex> = LazyLock::new(|| re(r"(\d{1,3}(?:,\d{3})+|\d{3,8})\s*원"));
static KRW_CUE: LazyLock<Regex> =
    LazyLock::new(|| re(r"할인|무료|적립|혜택|요금|월|결제|쿠폰|캐시백|지원"));
static FREE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)무료|\bfree\b"));
static NOT_FREE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)무료\s*(배송|반품|교환|샘플|템플릿|도안|폰트|이미지|다운로드|상담|주차)|free\s+(shipping|returns?|templates?|download)",
    )
});
static VERIFICATION: &[(&str, &[&str])] = &[
    ("sheerid", &["sheerid"]),
    ("unidays", &["unidays"]),
    ("student_beans", &["student beans", "studentbeans"]),
    (
        "github_student",
        &["github student", "student developer pack"],
    ),
    (
        "school_email",
        &[
            "학교 이메일",
            "학교 메일",
            "학교메일",
            "대학 이메일",
            "school email",
            "university email",
            "edu email",
            "ac.kr",
        ],
    ),
    (
        "student_id",
        &["학생증", "재학증명서", "재학 증명서", "student id"],
    ),
];
/// A template slot left unfilled in the visible text (`{{product-name}}`).
static TEMPLATE_SLOT: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{\s*[\w.-]+\s*\}\}"));
static PRODUCT_PAGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"ISBN|정가\s*[:：]?\s*[\d,]+\s*원|판매가\s*[:：]?\s*[\d,]+\s*원"));
static FULL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(20\d{2})\s*(?:[./\-]|년)\s*(\d{1,2})\s*(?:[./\-]|월)\s*(\d{1,2})\s*일?")
});
static PERIOD_CUE: LazyLock<Regex> = LazyLock::new(|| re(r"기간|까지|~|∼|종료|마감|until|through"));
static ENDED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"종료된\s*이벤트|이벤트가\s*종료|행사가\s*종료|종료되었습니다|마감되었습니다|판매\s*종료|this (offer|promotion) has ended|offer (has )?expired",
    )
});

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    pub kind: &'static str,
    pub value: Value,
    pub quote: String,
    /// `read_quote` of the quote: what P1 evidence will check.
    pub evidence_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Offer {
    pub facts: Vec<Fact>,
    pub audience: Vec<&'static str>,
    pub verification: Option<Fact>,
    pub end_date: Option<NaiveDate>,
    pub offer_lines: Vec<String>,
    pub age: Option<(Option<i32>, Option<i32>)>,
    /// The free period is labelled a trial (rule 4): shown with its term.
    pub trial: bool,
    /// Brand scope: the issuer the offer's own lines name (review H6), its
    /// sponsor and holder requirement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<OfferIssuer>,
    /// Brand scope: the door brand the offer was read for (its redeem
    /// brand). `None` for a student or youth offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_brand: Option<String>,
}

/// The issuer a brand-scope offer names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OfferIssuer {
    pub brand_id: uuid::Uuid,
    pub name: String,
    /// `field:value`.
    pub holder: String,
    /// The visible line that names it, verbatim (at most
    /// [`MAX_QUOTE_CHARS`] characters): the restriction shown with the
    /// condition template.
    pub line: String,
}

fn char_index(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

/// A quote of at most [`MAX_QUOTE_CHARS`] characters from `line` that
/// contains the byte span `start..end` (clipped to the limit).
pub fn quote_around(line: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX_QUOTE_CHARS {
        return line.trim().to_string();
    }
    let s = char_index(line, start);
    let e = char_index(line, end).max(s + 1).min(chars.len());
    let span = e - s;
    if span >= MAX_QUOTE_CHARS {
        return chars[s..s + MAX_QUOTE_CHARS]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
    }
    let room = MAX_QUOTE_CHARS - span;
    let mut from = s.saturating_sub(room / 2);
    let mut to = (from + MAX_QUOTE_CHARS).min(chars.len());
    if to - from < MAX_QUOTE_CHARS {
        from = to.saturating_sub(MAX_QUOTE_CHARS);
    }
    to = to.min(from + MAX_QUOTE_CHARS);
    chars[from..to]
        .iter()
        .collect::<String>()
        .trim()
        .to_string()
}

pub(crate) fn audience_codes(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    AUDIENCE_CODES
        .iter()
        .filter(|(_, words)| words.iter().any(|w| lower.contains(w)))
        .map(|(code, _)| *code)
        .collect()
}

fn dates_in(text: &str) -> Vec<NaiveDate> {
    FULL_DATE
        .captures_iter(text)
        .filter_map(|c| {
            NaiveDate::from_ymd_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)
        })
        .collect()
}

fn make_fact(
    line: &str,
    target: (usize, usize),
    kind: &'static str,
    value: Value,
    start: usize,
    end: usize,
) -> Fact {
    // The quote covers the audience cue and the fact when both fit.
    let (s, e) = (start.min(target.0), end.max(target.1));
    let quote = if char_index(line, e) - char_index(line, s) <= MAX_QUOTE_CHARS {
        quote_around(line, s, e)
    } else {
        quote_around(line, start, end)
    };
    let (evidence_type, _) = read_quote(&quote);
    Fact {
        kind,
        value,
        quote,
        evidence_type,
    }
}

/// What one line states for students: its facts, and whether its free
/// period is labelled a trial.
#[derive(Debug, Default)]
struct LineRead {
    facts: Vec<Fact>,
    trial: bool,
}

/// A free claim (`free`, `무료`) of `clause` outside link text that is not a
/// free extra (`free shipping`): its byte offset in `clause`.
fn free_claim(clause: &str, clause_at: usize, line: &Line) -> Option<(usize, usize)> {
    if NOT_FREE.is_match(clause) || verification_step(clause) {
        return None;
    }
    FREE.find_iter(clause)
        .map(|m| (clause_at + m.start(), clause_at + m.end()))
        .find(|(at, _)| !line.in_link(*at))
}

/// The audiences a sentence of `line` is read with: its own, or else the
/// heading's when the sentence sits under a heading read with the line.
fn sentence_audiences(line: &Line, (start, end): (usize, usize)) -> Vec<(usize, usize, Party)> {
    let own: Vec<(usize, usize, Party)> = audience_mentions(&line.text[start..end])
        .into_iter()
        .map(|(s, e, p)| (s + start, e + start, p))
        .collect();
    if !own.is_empty() || line.heading == 0 || start < line.heading {
        return own;
    }
    audience_mentions(&line.text[..line.heading])
}

/// Facts of one offer line: every value ([`super::values`]) that rule 1
/// binds to a student or youth audience of its own sentence (or of the
/// heading it sits under), outside link text, in a sentence whose subject
/// is no verification programme. A free claim takes the one free period of
/// its clause ([`super::values::free_terms`]); a trial clause with no
/// period states no offer.
fn line_facts(line: &Line) -> LineRead {
    let mut read = LineRead::default();
    let text = line.text.as_str();
    let mut free_claims: Vec<Fact> = Vec::new();
    let mut terms: Vec<(super::values::Term, Fact, bool)> = Vec::new();
    // The line names a student or youth audience as a benefit's target;
    // each value is then bound to its own sentence's audience (rule 1).
    if find_target(text).is_none() {
        return read;
    }
    for (start, end) in sentence_spans(text) {
        let sentence = &text[start..end];
        if someone_elses(line, (start, end)) {
            continue;
        }
        let audiences = sentence_audiences(line, (start, end));
        if audiences.is_empty() {
            continue;
        }
        // A fact's quote stays inside its own sentence (with the heading
        // when the audience is the heading's).
        let fact =
            |target: (usize, usize), kind: &'static str, value: Value, at: (usize, usize)| {
                let from = start.min(target.0);
                make_fact(
                    &text[from..end],
                    (target.0 - from, target.1 - from),
                    kind,
                    value,
                    at.0 - from,
                    at.1 - from,
                )
            };
        let student_target = |at: (usize, usize)| -> Option<(usize, usize)> {
            if line.in_link(at.0) || !bound_to_students(text, &audiences, at) {
                return None;
            }
            audiences
                .iter()
                .min_by_key(|(s, e, _)| {
                    if *e <= at.0 {
                        at.0 - e
                    } else {
                        s.saturating_sub(at.1)
                    }
                })
                .map(|(s, e, _)| (*s, *e))
        };
        for (s, e, n) in super::values::discount_percents(sentence) {
            if let Some(target) = student_target((start + s, start + e)) {
                read.facts.push(fact(
                    target,
                    "discount_percent",
                    json!(n),
                    (start + s, start + e),
                ));
            }
        }
        if KRW_CUE.is_match(sentence) {
            for c in KRW.captures_iter(sentence) {
                let m = c.get(0).expect("match");
                let value: u64 = c[1].replace(',', "").parse().unwrap_or(0);
                if !(100..=10_000_000).contains(&value) {
                    continue;
                }
                if let Some(target) = student_target((start + m.start(), start + m.end())) {
                    read.facts.push(fact(
                        target,
                        "krw",
                        json!(value),
                        (start + m.start(), start + m.end()),
                    ));
                }
            }
        }
        for (offset, clause) in super::values::clauses(sentence) {
            let clause_at = start + offset;
            let Some(claim) = free_claim(clause, clause_at, line) else {
                continue;
            };
            let Some(target) = student_target(claim) else {
                continue;
            };
            let span = (clause_at, clause_at + clause.len());
            let found = super::values::free_terms(clause);
            if found.is_empty() {
                if !super::values::is_trial(clause) {
                    free_claims.push(fact(target, "free_access", json!(true), claim));
                }
                continue;
            }
            for (_, term, trial) in found {
                let (kind, value) = term_kind(term);
                terms.push((term, fact(target, kind, value, span), trial));
            }
        }
    }
    let distinct: BTreeSet<super::values::Term> = terms.iter().map(|(t, ..)| *t).collect();
    for term in &distinct {
        if let Some((_, fact, _)) = terms.iter().find(|(t, ..)| t == term) {
            read.facts.push(fact.clone());
        }
    }
    read.trial = terms.iter().any(|(.., trial)| *trial);
    if distinct.is_empty() {
        if let Some(fact) = free_claims.into_iter().next() {
            read.facts.push(fact);
        }
    }
    if text.contains('세') && find_target(text).is_some() {
        if let Ok(age) = parse_age(text) {
            if age.max.is_some_and(|max| max <= 39) {
                let at = text.find('세').unwrap_or(0);
                let start = text[..at]
                    .char_indices()
                    .rev()
                    .nth(11)
                    .map_or(0, |(i, _)| i);
                let end = at + '세'.len_utf8();
                let target = find_target(text).expect("checked");
                read.facts.push(make_fact(
                    text,
                    target,
                    "age_band",
                    json!({"min": age.min, "max": age.max}),
                    start,
                    end,
                ));
            }
        }
    }
    read
}

/// A free period's fact kind and value: `free_months` for whole months,
/// `free_days` otherwise.
fn term_kind(term: super::values::Term) -> (&'static str, Value) {
    match term {
        super::values::Term::Months(months) => ("free_months", json!(months)),
        super::values::Term::Days(days) => ("free_days", json!(days)),
        super::values::Term::Enrolled => ("free_enrolled", json!(true)),
    }
}

/// The subscription-length periods (months or years) `line` states
/// outside its link text and cadences that rule 1 binds to students, with a
/// fact quoting its sentence (another audience's are skipped); `None` when
/// one names no audience at all.
fn stated_periods(line: &Line) -> Option<Vec<(super::values::Term, Fact)>> {
    let mut out = Vec::new();
    for (start, end) in sentence_spans(&line.text) {
        let sentence = &line.text[start..end];
        let audiences = sentence_audiences(line, (start, end));
        for (offset, clause) in super::values::clauses(sentence) {
            let at = (start + offset, start + offset + clause.len());
            if line.in_link(at.0) {
                continue;
            }
            let readable = super::values::without_cadence(clause);
            for concept in super::values::concepts(&readable) {
                // Days and weeks next to an offer are a feature's (`30-day
                // page history`, `14 days retention`), not its term.
                if concept.ends_with("day") || concept.ends_with("week") {
                    continue;
                }
                let Some(term) = super::values::Term::parse(&concept) else {
                    continue;
                };
                // Another audience's period (`Teachers: … a two-year
                // subscription`) is theirs; one that names no one is unknown.
                if audiences.is_empty() {
                    return None;
                }
                if !bound_to_students(&line.text, &audiences, at) {
                    continue;
                }
                let (kind, value) = term_kind(term);
                let span = (offset, offset + clause.len());
                out.push((term, make_fact(sentence, span, kind, value, span.0, span.1)));
            }
        }
    }
    Some(out)
}

/// How many lines from an offer line a period that names no one may sit:
/// an offer card's own lines (miro.com: `Free`, `Apply as student`, `What's
/// included:`, `10 team members`, `2 years`).
const NEAR_LINES: usize = 8;

/// A free period the page states, and whether rule 1 binds it to students
/// (otherwise its sentence names no audience).
struct PageTerm {
    term: super::values::Term,
    fact: Fact,
    trial: bool,
    bound: bool,
}

/// Rule 4 (term): every free period `segments` state for this offer: in a
/// clause that claims it free, outside link text, in a sentence whose
/// subject is no verification programme, bound to students by rule 1 where
/// the sentence names an audience; a sentence that names none counts unless
/// its clause is a trial (a trial open to everyone is no term of this offer).
fn page_terms(segments: &[Line], offer_at: &BTreeSet<usize>) -> Vec<PageTerm> {
    let mut out = Vec::new();
    for (index, line) in segments.iter().enumerate() {
        // A sentence that names no one belongs to the offer only next to
        // the offer's own lines (Sentry's plan list, not a paid edition's
        // `one year of free updates` further down).
        let near = offer_at.iter().any(|at| at.abs_diff(index) <= NEAR_LINES);
        let text = line.text.as_str();
        for (start, end) in sentence_spans(text) {
            let sentence = &text[start..end];
            if someone_elses(line, (start, end)) {
                continue;
            }
            let audiences = sentence_audiences(line, (start, end));
            for (offset, clause) in super::values::clauses(sentence) {
                let clause_at = start + offset;
                let Some(claim) = free_claim(clause, clause_at, line) else {
                    continue;
                };
                for (_, term, trial) in super::values::free_terms(clause) {
                    let bound = !audiences.is_empty() && bound_to_students(text, &audiences, claim);
                    if bound || (audiences.is_empty() && !trial && near) {
                        let (kind, value) = term_kind(term);
                        let span = (offset, offset + clause.len());
                        let fact = make_fact(sentence, span, kind, value, span.0, span.1);
                        out.push(PageTerm {
                            term,
                            fact,
                            trial,
                            bound,
                        });
                    }
                }
            }
        }
    }
    out
}

/// A discount cue (brand scope): the clause says the value is taken off.
static DISCOUNT_CUE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)할인|세일|\boff\b|\bdiscount(?:s|ed)?\b"));
/// A benefit cue a brand-scope line must carry at all.
static BENEFIT_CUE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)할인|적립|쿠폰|캐시백|무료|증정|\boff\b|\bdiscount"));
/// "최대 N": an upper bound a single page states as a headline, never one
/// offer's value under brand scope.
static UPPER_BOUND: LazyLock<Regex> = LazyLock::new(|| re(r"최대\s*$|up\s+to\s*$"));
/// A per-amount rate (`1천원당 100원`): never a flat amount.
static PER_AMOUNT: LazyLock<Regex> = LazyLock::new(|| re(r"(?:\d|천|만)\s*원\s*당"));
/// A value is the discount only when the cue is bound to it: right after
/// it (`10% 할인`, `1,000원 즉시할인`) or right before it (`할인율 : 20%`).
/// A threshold (`1천원부터 할인`, `3만원 이상 결제 시 할인`) is not.
static CUE_AFTER: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^\s*(?:[가-힣]{1,3}\s*)?(?:할인|세일|off\b|discount)"));
/// A threshold word right after the value: the value bounds the spend,
/// it is not taken off.
static THRESHOLD_AFTER: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:부터|이상|이하|미만|초과|까지)"));
static CUE_BEFORE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:할인(?:율|액|금액)?|discount)\s*[:：]?\s*$"));

/// Brand scope's reader of one line: every percent or won value in a
/// clause with a discount cue (not 적립 alone), outside link text, not an
/// upper bound and not a per-amount rate, in a sentence whose subject is no
/// verification programme.
fn brand_line_facts(line: &Line) -> LineRead {
    let mut read = LineRead::default();
    let text = line.text.as_str();
    if !BENEFIT_CUE.is_match(text) {
        return read;
    }
    for (start, end) in sentence_spans(text) {
        if someone_elses(line, (start, end)) {
            continue;
        }
        let sentence = &text[start..end];
        for (offset, clause) in super::values::clauses(sentence) {
            let Some(cue) = DISCOUNT_CUE.find(clause) else {
                continue;
            };
            if PER_AMOUNT.is_match(clause) {
                continue;
            }
            let at = start + offset;
            let cue_span = (at + cue.start(), at + cue.end());
            for (s, e, token) in super::values::value_spans(clause) {
                let (kind, value) = if let Some(n) = token.strip_suffix('%') {
                    match n.parse::<u32>() {
                        Ok(n) if (1..=90).contains(&n) => ("discount_percent", json!(n)),
                        _ => continue,
                    }
                } else if let Some(n) = token.strip_suffix('원') {
                    match n.parse::<u64>() {
                        Ok(n) if (100..=10_000_000).contains(&n) => ("krw", json!(n)),
                        _ => continue,
                    }
                } else {
                    continue;
                };
                if line.in_link(at + s) || UPPER_BOUND.is_match(&clause[..s]) {
                    continue;
                }
                let bound = (CUE_AFTER.is_match(&clause[e..])
                    && !THRESHOLD_AFTER.is_match(&clause[e..]))
                    || CUE_BEFORE.is_match(&clause[..s]);
                if !bound {
                    continue;
                }
                let from = start.min(cue_span.0);
                read.facts.push(make_fact(
                    &text[from..end],
                    (cue_span.0 - from, cue_span.1 - from),
                    kind,
                    value,
                    at + s - from,
                    at + e - from,
                ));
            }
        }
    }
    read
}

fn read_line(line: &Line, scope: &Scope) -> LineRead {
    match scope {
        Scope::Audience => line_facts(line),
        Scope::Brand(_) => brand_line_facts(line),
    }
}

/// How far from an offer line an issuer name binds it: the line itself, a
/// short line just after it (`10% 할인` / `대상: T 멤버십 할인형 고객`) and
/// up to two short heading lines before it.
const ISSUER_SHORT_LINE: usize = 40;

/// The issuer an offer's own lines name (review H6): `Err` when they name
/// more than one. `lines` are the page's visible lines, indexed as the
/// segments the offer was read from. Of the lines naming the issuer, one
/// after the offer line is preferred (`10% 할인` / `대상: T 멤버십 할인형
/// 고객` states who gets it), then the line itself, then a heading above.
fn offer_issuer(
    scope: &BrandScope,
    lines: &[Line],
    offer_at: &BTreeSet<usize>,
) -> Result<Option<OfferIssuer>, &'static str> {
    let registry = scope.issuer_registry();
    let short = |i: usize| lines[i].text.chars().count() <= ISSUER_SHORT_LINE;
    // issuer -> (priority, line index, issuer)
    let mut named: BTreeMap<uuid::Uuid, (u8, usize, OfferIssuer)> = BTreeMap::new();
    for &at in offer_at.iter().filter(|at| **at < lines.len()) {
        let mut window = vec![(1u8, at)];
        if at + 1 < lines.len() && short(at) {
            window.push((0, at + 1));
        }
        for back in 1..=2 {
            if at >= back && (1..=back).all(|b| short(at - b)) {
                window.push((2, at - back));
            }
        }
        for (priority, i) in window {
            // Link text names another page (a tab to the KT page), never
            // this offer's issuer.
            let own = lines[i].without_links();
            for issuer in registry.issuers_in(&own) {
                if issuer.brand_id == scope.brand_id {
                    continue;
                }
                let Some(holder) = issuer.holder else {
                    continue;
                };
                let found = OfferIssuer {
                    brand_id: issuer.brand_id,
                    name: issuer.name.clone(),
                    holder: format!("{}:{}", holder.field.name(), holder.value),
                    line: own.trim().chars().take(MAX_QUOTE_CHARS).collect(),
                };
                let better = named
                    .get(&issuer.brand_id)
                    .is_none_or(|(p, at, _)| (priority, i) < (*p, *at));
                if better {
                    named.insert(issuer.brand_id, (priority, i, found));
                }
            }
        }
    }
    if named.len() > 1 {
        return Err("conflicting_issuers");
    }
    Ok(named.into_values().next().map(|(_, _, issuer)| issuer))
}

/// Days an article found through a list may be old (its publication date,
/// never its last modification), and how long after it its offer may end.
pub const ARTICLE_MAX_AGE_DAYS: i64 = 30;
pub const ARTICLE_MAX_SPAN_DAYS: i64 = 60;

/// A page a list found must be an article of now (review M3): published
/// (never "last modified") within [`ARTICLE_MAX_AGE_DAYS`], with an explicit
/// end date at most [`ARTICLE_MAX_SPAN_DAYS`] after publication. A yearless
/// period ("오는 6월 4일까지") never gives an end date, so it never passes.
pub fn dated_article(
    html: &str,
    end_date: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<(), &'static str> {
    let (published, _modified) = page_dates(html);
    let Some(published) = published.into_iter().filter(|d| *d <= today).max() else {
        return Err("article_undated");
    };
    if (today - published).num_days() > ARTICLE_MAX_AGE_DAYS {
        return Err("article_too_old");
    }
    let Some(end) = end_date else {
        return Err("article_without_end");
    };
    if end < today || (end - published).num_days() > ARTICLE_MAX_SPAN_DAYS {
        return Err("article_end_implausible");
    }
    Ok(())
}

/// Reads one student/youth offer from a page, or the reason it does not
/// qualify. Pure; `today` is the KST date of the run.
pub fn extract_offer(html: &str, today: NaiveDate) -> Result<Offer, &'static str> {
    extract_offer_scoped(html, today, &Scope::Audience)
}

/// [`extract_offer`] under `scope`: the audience gate of [`Scope::Audience`]
/// or the discount cue of [`Scope::Brand`]; every other guard is the same.
pub fn extract_offer_scoped(
    html: &str,
    today: NaiveDate,
    scope: &Scope,
) -> Result<Offer, &'static str> {
    let text = visible_text(html);
    if text.chars().count() < JS_SHELL_CHARS {
        return Err("js_shell");
    }
    // An unrendered template (`Not included in {{creative-cloud-pro}}`)
    // says what the page shows only once a script fills it in.
    if TEMPLATE_SLOT.is_match(&text) {
        return Err("template_shell");
    }
    if ENDED.is_match(&text) {
        return Err("offer_ended");
    }
    // Product detail pages (books, goods) carry list prices and per-item
    // discounts next to titles; they are never a student offer page.
    if PRODUCT_PAGE.is_match(&text) {
        return Err("product_listing");
    }
    let linked = visible_lines_linked(html);
    let lines: Vec<String> = linked.iter().map(|line| line.text.clone()).collect();
    // Short heading lines are read together with the line that follows
    // ("학생 할인" + "10% 할인"); both are contiguous in the visible text.
    // A question heading (a FAQ's `How long can I use it?`) labels nothing:
    // the answer under it is read on its own.
    let mut segments: Vec<Line> = Vec::new();
    for (i, line) in linked.iter().enumerate() {
        if line.text.chars().count() <= 40
            && read_line(line, scope).facts.is_empty()
            && !super::publish::is_question(&line.text)
        {
            if let Some(next) = linked.get(i + 1) {
                segments.push(Line::headed(line, next));
                continue;
            }
        }
        segments.push(line.clone());
    }
    let mut facts: Vec<Fact> = Vec::new();
    let mut offer_lines: Vec<String> = Vec::new();
    let mut trial = false;
    let mut offer_at: BTreeSet<usize> = BTreeSet::new();
    for (index, segment) in segments.iter().enumerate() {
        let found = read_line(segment, scope);
        if found.facts.is_empty() {
            continue;
        }
        offer_at.insert(index);
        trial |= found.trial;
        offer_lines.push(segment.text.chars().take(MAX_QUOTE_CHARS).collect());
        for fact in found.facts {
            if !facts
                .iter()
                .any(|f| f.kind == fact.kind && f.value == fact.value)
            {
                facts.push(fact);
            }
        }
    }
    if facts.is_empty() {
        return Err(match scope {
            Scope::Audience => "no_student_youth_offer",
            Scope::Brand(_) => "no_brand_offer",
        });
    }
    if stale_page(html, today) {
        return Err("stale_page");
    }
    let percents: BTreeSet<String> = facts
        .iter()
        .filter(|f| f.kind == "discount_percent")
        .map(|f| f.value.to_string())
        .collect();
    let krws: BTreeSet<String> = facts
        .iter()
        .filter(|f| f.kind == "krw")
        .map(|f| f.value.to_string())
        .collect();
    let ages: BTreeSet<String> = facts
        .iter()
        .filter(|f| f.kind == "age_band")
        .map(|f| f.value.to_string())
        .collect();
    let periods: BTreeSet<String> = facts
        .iter()
        .filter(|f| f.kind == "free_months")
        .map(|f| f.value.to_string())
        .collect();
    // Several offers are several values. Lines that restate the page's one
    // value are one offer; lines that state no value at all ("free" alone)
    // cannot be told from a list of free things, so their number still counts.
    let values = percents.len() + krws.len() + periods.len();
    if percents.len() > 2
        || krws.len() > 4
        || ages.len() > 1
        || periods.len() > 1
        || (values == 0 && offer_lines.len() > 8)
    {
        return Err("multi_offer_page");
    }
    // Two different age readings among the quotes, or among the page's
    // audience lines, would contradict the programme's single age band.
    let quote_ages: BTreeSet<(Option<i32>, Option<i32>)> = facts
        .iter()
        .filter_map(|f| parse_age(&f.quote).ok().map(|a| (a.min, a.max)))
        .collect();
    let page_ages: BTreeSet<(Option<i32>, Option<i32>)> = lines
        .iter()
        .filter(|l| l.contains('세') && find_target(l).is_some())
        .filter_map(|l| parse_age(l).ok().map(|a| (a.min, a.max)))
        .collect();
    if quote_ages.len() > 1
        || page_ages.len() > 1
        || !quote_ages.is_subset(&page_ages) && !page_ages.is_empty()
    {
        return Err("conflicting_ages");
    }
    // Rule 4: a free offer carries the one free period the page states for
    // it; two different periods are two offers, and a period under a month
    // is a trial, never an offer.
    if facts.iter().any(|f| {
        matches!(
            f.kind,
            "free_access" | "free_months" | "free_days" | "free_enrolled"
        )
    }) {
        let stated = page_terms(&segments, &offer_at);
        // Periods bound to students, and the facts' own, must agree.
        let mut terms: BTreeSet<super::values::Term> =
            stated.iter().filter(|t| t.bound).map(|t| t.term).collect();
        for fact in &facts {
            match (fact.kind, fact.value.as_u64()) {
                ("free_months", Some(n)) => terms.insert(super::values::Term::Months(n as u32)),
                ("free_days", Some(n)) => terms.insert(super::values::Term::Days(n as u32)),
                ("free_enrolled", _) => terms.insert(super::values::Term::Enrolled),
                _ => false,
            };
        }
        if terms.len() > 1 {
            return Err("conflicting_terms");
        }
        // A free offer with no period of its own takes the one period the
        // page states for it, bound or in a sentence that names no one.
        if terms.is_empty() {
            let all: BTreeSet<super::values::Term> = stated.iter().map(|t| t.term).collect();
            if all.len() > 1 {
                return Err("conflicting_terms");
            }
            if let Some(found) = stated.into_iter().next() {
                trial |= found.trial;
                terms.insert(found.term);
                facts.push(found.fact);
            }
        }
        if terms.iter().any(|t| t.under_a_month()) {
            return Err("short_free_term");
        }
        // Rule 7: a free offer is shown with its term or not at all. A
        // subscription-length period stated next to it for students (Axure:
        // `Students: … request a one-year Axure RP subscription`) is its
        // term; one that names no one (a pricing cell `2 years`, a bundle's
        // `+1 year`) leaves the term unknown. A period under a month there is
        // a feature (`30-day page history`), not a term.
        if terms.is_empty() {
            let mut near = Vec::new();
            for (index, line) in segments.iter().enumerate() {
                if offer_at.iter().any(|at| at.abs_diff(index) <= NEAR_LINES) {
                    near.extend(stated_periods(line).ok_or("unbound_term")?);
                }
            }
            let distinct: BTreeSet<super::values::Term> = near.iter().map(|(t, _)| *t).collect();
            if distinct.len() > 1 {
                return Err("conflicting_terms");
            }
            if let Some((_, fact)) = near.into_iter().next() {
                facts.push(fact);
            }
        }
    }
    let end_date = lines
        .iter()
        .filter(|l| PERIOD_CUE.is_match(l))
        .flat_map(|l| dates_in(l))
        .max();
    if end_date.is_some_and(|end| end < today) {
        return Err("offer_ended");
    }
    let verification = VERIFICATION.iter().find_map(|(code, words)| {
        lines.iter().find_map(|line| {
            let lower = line.to_lowercase();
            let word = words.iter().find(|w| lower.contains(*w))?;
            // Lowercasing keeps byte offsets only for ASCII; the vocabulary
            // is ASCII or Hangul (unchanged by lowercasing), so the offset holds.
            let pos = lower.find(word)?;
            if !line.is_char_boundary(pos) || !line.is_char_boundary(pos + word.len()) {
                return None;
            }
            let quote = quote_around(line, pos, pos + word.len());
            let (evidence_type, _) = read_quote(&quote);
            Some(Fact {
                kind: "verification",
                value: json!(code),
                quote,
                evidence_type,
            })
        })
    });
    let audience = audience_codes(&offer_lines.join(" "));
    let issuer = match scope {
        Scope::Audience => None,
        Scope::Brand(brand) => offer_issuer(brand, &linked, &offer_at)?,
    };
    if let Scope::Brand(brand) = scope {
        // Code review C-2 (3): one issuer, one value; two values are two
        // offers (a tier table "VIP 30% / 골드 20%" is not one).
        if percents.len() + krws.len() > 1 {
            return Err("multi_offer_page");
        }
        // Code review H-6: offer lines with different subjects ("아메리카노
        // 10% 할인" / "케이크 10% 할인") are several offers, not one. The
        // subject is what a line says before its value; a heading read
        // with the line ("상시 혜택" + "10% 할인") restates, never adds one.
        let subjects: BTreeSet<String> = linked
            .iter()
            .filter(|line| !read_line(line, scope).facts.is_empty())
            .filter_map(|line| {
                let text = line.text.as_str();
                let (start, _, _) = super::values::value_spans(text)
                    .into_iter()
                    .find(|(_, _, t)| t.ends_with('%') || t.ends_with('원'))?;
                Some(text[..start].split_whitespace().collect::<String>())
            })
            .collect();
        if subjects.len() > 1 {
            return Err("multi_offer_page");
        }
        // Code review C-2 (2): fail closed. Every issuer the page's own
        // text names (links excluded) must be the one the offer binds.
        let registry = brand.issuer_registry();
        let bound = issuer.as_ref().map(|i| i.brand_id);
        for line in &linked {
            for named in registry.issuers_in(&line.without_links()) {
                if named.brand_id != brand.brand_id && Some(named.brand_id) != bound {
                    return Err("issuer_unbound");
                }
            }
        }
        // Code review H-6: a period without its year ("10월 1일 ~ 10월 31일")
        // never gives an end date, so it refuses on every brand-scope page.
        for line in lines
            .iter()
            .filter(|l| PERIOD_CUE.is_match(l) || offer_lines.iter().any(|o| o == *l))
        {
            if super::doors::rows::row_period(line, today) == Err("period_without_year") {
                return Err("period_without_year");
            }
        }
        if brand.dated {
            dated_article(html, end_date, today)?;
        }
    }
    let scope_brand = match scope {
        Scope::Audience => None,
        Scope::Brand(brand) => Some(brand.brand_name.clone()),
    };
    Ok(Offer {
        issuer,
        scope_brand,
        trial,
        age: quote_ages.into_iter().next(),
        facts,
        audience,
        verification,
        end_date,
        offer_lines: offer_lines.into_iter().take(5).collect(),
    })
}

// ── staleness and region (rules 5 and 6) ────────────────────────────────

/// How old a page's own newest date may be.
pub const STALE_AFTER_DAYS: i64 = 365;

static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(\d{4})-(\d{2})-(\d{2})"));
static WRITTEN_DATE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^\s*(?:(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2}),?\s+(\d{4})|(\d{1,2})\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?,?\s+(\d{4})|(\d{4})\s*[./\-년]\s*(\d{1,2})\s*[./\-월]\s*(\d{1,2})\s*일?)\s*$",
    )
});

fn month_number(name: &str) -> Option<u32> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = name.to_ascii_lowercase();
    months
        .iter()
        .position(|m| lower.starts_with(m))
        .map(|i| i as u32 + 1)
}

/// A date written whole: ISO (`2020-01-19T…`), `Jan 19, 2020`,
/// `19 January 2020`, `2020.01.19`, `2020년 1월 19일`.
fn written_date(text: &str) -> Option<NaiveDate> {
    if let Some(c) = ISO_DATE.captures(text) {
        return NaiveDate::from_ymd_opt(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?);
    }
    let c = WRITTEN_DATE.captures(text)?;
    let (year, month, day) = if let Some(month) = c.get(1) {
        (
            c[3].parse().ok()?,
            month_number(month.as_str())?,
            c[2].parse().ok()?,
        )
    } else if let Some(day) = c.get(4) {
        (
            c[6].parse().ok()?,
            month_number(&c[5])?,
            day.as_str().parse().ok()?,
        )
    } else {
        (c[7].parse().ok()?, c[8].parse().ok()?, c[9].parse().ok()?)
    };
    NaiveDate::from_ymd_opt(year, month, day)
}

/// The page's own dates: when it was published (JSON-LD `datePublished`,
/// `article:published_time`, a `<time datetime>` in an article or header,
/// a byline element whose class names a date and whose whole text is one)
/// and when it was last modified (JSON-LD `dateModified`,
/// `article:modified_time`, `og:updated_time`).
pub fn page_dates(html: &str) -> (Vec<NaiveDate>, Vec<NaiveDate>) {
    static LD: LazyLock<Regex> =
        LazyLock::new(|| re(r#""date(Published|Modified)"\s*:\s*"([^"]{8,40})""#));
    static ARTICLE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r#""@type"\s*:\s*\[?\s*"(?:Article|BlogPosting|NewsArticle|TechArticle|Report|AnalysisNewsArticle)""#,
        )
    });
    let mut published = Vec::new();
    let mut modified = Vec::new();
    // Structured data dates an article; a web page's own CMS dates do not.
    let article = ARTICLE.is_match(html);
    for c in LD.captures_iter(html).filter(|_| article) {
        if let Some(date) = written_date(&c[2]) {
            if &c[1] == "Published" {
                published.push(date);
            } else {
                modified.push(date);
            }
        }
    }
    let document = Html::parse_document(html);
    let select = |css: &str| scraper::Selector::parse(css).expect("valid date selector");
    for meta in document.select(&select("meta[content]")) {
        let key = ["property", "name", "itemprop"]
            .iter()
            .filter_map(|a| meta.value().attr(a))
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        let Some(date) = meta.value().attr("content").and_then(written_date) else {
            continue;
        };
        if key.contains("published_time") || key.contains("datepublished") {
            published.push(date);
        } else if key.contains("modified_time")
            || key.contains("updated_time")
            || key.contains("datemodified")
        {
            modified.push(date);
        }
    }
    for time in document.select(&select(
        "article time[datetime], header time[datetime], time[pubdate], time[itemprop=datePublished]",
    )) {
        if let Some(date) = time.value().attr("datetime").and_then(written_date) {
            published.push(date);
        }
    }
    for element in document.select(&select("[class]")) {
        let class = element
            .value()
            .attr("class")
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(class.contains("date") || class.contains("publish") || class.contains("posted")) {
            continue;
        }
        let text: String = element
            .text()
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ");
        if text.chars().count() <= 40 {
            if let Some(date) = written_date(&text) {
                published.push(date);
            }
        }
    }
    // A labelled byline in the visible text (`게시일 2023.05.01`,
    // `등록일: 2023-05-01`, `Posted on Jan 19, 2020`).
    static BYLINE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?i)^(?:게시일|등록일|작성일|공지일|posted\s+on|published(?:\s+on)?)\s*[:：]?\s*(.{6,30})$",
        )
    });
    static UPDATED: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)^(?:수정일|최종\s*수정일|updated(?:\s+on)?|last\s+updated)\s*[:：]?\s*(.{6,30})$")
    });
    for line in visible_lines(html) {
        if let Some(date) = BYLINE.captures(&line).and_then(|c| written_date(&c[1])) {
            published.push(date);
        } else if let Some(date) = UPDATED.captures(&line).and_then(|c| written_date(&c[1])) {
            modified.push(date);
        }
    }
    (published, modified)
}

/// Rule 5 (staleness): a page that states when it was published, whose
/// newest own date (publication or modification) is more than
/// [`STALE_AFTER_DAYS`] before `today`. A page that states no publication
/// date (a maintained landing page) is not dated by its last edit alone.
pub fn stale_page(html: &str, today: NaiveDate) -> bool {
    let (published, modified) = page_dates(html);
    let Some(newest) = published.iter().chain(modified.iter()).max() else {
        return false;
    };
    !published.is_empty() && (today - *newest).num_days() > STALE_AFTER_DAYS
}

/// Countries by name, and whether the name is Korea's. Upper-case
/// abbreviations are matched as written (`US`, never `us`).
const COUNTRIES: &[(&str, bool)] = &[
    (
        r"(?i:(?:south\s+)?korea|republic\s+of\s+korea)|대한민국|한국|국내",
        true,
    ),
    (
        r"(?i:united\s+states(?:\s+of\s+america)?|u\.s\.(?:a\.)?|america)|\bUSA?\b|미국",
        false,
    ),
    (
        r"(?i:united\s+kingdom|great\s+britain|britain|england)|\bUK\b|영국",
        false,
    ),
    (r"(?i:canada)|캐나다", false),
    (r"(?i:australia|new\s+zealand)|호주|뉴질랜드", false),
    (
        r"(?i:ireland|india|germany|france|spain|portugal|italy|netherlands|belgium|sweden|norway|denmark|finland|switzerland|austria|poland)|독일|프랑스",
        false,
    ),
    (
        r"(?i:japan|china|taiwan|hong\s+kong|singapore)|일본|중국|대만|홍콩|싱가포르",
        false,
    ),
    (r"(?i:brazil|mexico)", false),
    (r"(?i:european\s+union|europe)|\bEU\b|유럽", false),
];

static COUNTRY: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"\b(?:the\s+)?(?:{})", country_alternation())));

fn is_korea(name: &str) -> bool {
    COUNTRIES
        .iter()
        .find(|(pattern, _)| re(&format!("^(?:{pattern})$")).is_match(name.trim()))
        .is_some_and(|(_, korea)| *korea)
}

/// `America` as part of a continent's name (`Latin America`, `North
/// America`), not the country.
fn continent(text: &str, start: usize, name: &str) -> bool {
    static BEFORE: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)\b(?:latin|south|north|central)\s+$"));
    name.to_ascii_lowercase().ends_with("america") && BEFORE.is_match(&text[..start])
}

/// Countries named in `text`, each with whether it is Korea.
fn countries_in(text: &str) -> Vec<bool> {
    COUNTRY
        .find_iter(text)
        .filter(|m| !continent(text, m.start(), m.as_str()))
        .map(|m| {
            let name = m.as_str().trim();
            let name = name
                .strip_prefix("the ")
                .or_else(|| name.strip_prefix("The "))
                .unwrap_or(name);
            is_korea(name)
        })
        .collect()
}

/// An availability statement (`available in`, `available for students
/// within`, `valid in`, `open to residents of`): the countries after it are
/// where the offer may be used.
static AVAILABILITY: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:available|valid|offered|open|eligible|redeemable|applies)\b[^.;]{0,60}?\b(?:in|within|to)\b|\b(?:residents?|citizens?)\s+of\b|에서만|거주자",
    )
});
/// A country as the only place of an offer (`only in the US`, `US only`,
/// `U.S. residents`), anywhere in the offer's section.
static PLACE_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\bonly\s+(?:in|for|to)\s+(?:the\s+)?(?:{c})|(?:{c})\s*(?:-|\s)\s*(?:only|residents?|citizens?)\b",
        c = country_alternation()
    ))
});
/// A country as the place of the offer in the offer's own sentence
/// (`save 15% to top destinations across America`, `students in the US`).
static PLACE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\b(?:across|throughout|in|within)\s+(?:the\s+)?(?:{})",
        country_alternation()
    ))
});

fn country_alternation() -> String {
    COUNTRIES
        .iter()
        .map(|(p, _)| format!("(?:{p})"))
        .collect::<Vec<_>>()
        .join("|")
}
/// Another country's store (`미국 스토어`, `US store`, `the UK eStore`).
static COUNTRY_STORE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?:{})\s*(?:온라인\s*)?(?:\be-?store\b|\bstore\b|스토어|쇼핑몰)",
        country_alternation()
    ))
});
/// A store (`eStore`, `online store`, `스토어`).
static STORE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\be-?store\b|\bstore\b|\bshop\b|스토어|쇼핑몰"));
/// A locale path segment naming a country (`/en-us/`, `/de_de/`).
static LOCALE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^/[a-z]{2}[-_]([a-z]{2})(?:/|$)"));

/// Rule 6 (usable from Korea): why the offer is restricted to another
/// country, or `None`. The section's sentences name where the offer is
/// available and Korea is not among the places named; an offer sentence
/// places the offer in another country (`across America`, `US only`); or
/// the offer is a store's and the page's locale names another country.
/// A global offer on an `en-us` page that names no store stays in scope.
pub fn region_restriction(lines: &[String], page_url: &str, quotes: &[&str]) -> Option<String> {
    for line in lines {
        for (start, end) in sentence_spans(line) {
            let sentence = &line[start..end];
            if let Some(m) = AVAILABILITY.find(sentence) {
                let named = countries_in(&sentence[m.start()..]);
                if !named.is_empty() && !named.contains(&true) {
                    return Some("availability".into());
                }
            }
            if let Some(m) = COUNTRY_STORE.find(sentence) {
                if !countries_in(m.as_str()).contains(&true) {
                    return Some("country_store".into());
                }
            }
            if let Some(m) = PLACE_ONLY.find(sentence) {
                if !countries_in(m.as_str()).contains(&true) {
                    return Some("country_only".into());
                }
            }
        }
    }
    // The offer's own sentences: its quotes, and every sentence of the
    // section that states one of its values (`Students 17 - 24 save 15% to
    // top destinations across America` under the `Students Save 15%` quote).
    let values: BTreeSet<String> = quotes
        .iter()
        .flat_map(|q| super::publish::value_tokens(q))
        .collect();
    let stating = lines.iter().flat_map(|line| {
        sentence_spans(line)
            .into_iter()
            .map(move |(start, end)| &line[start..end])
    });
    let own = quotes.iter().copied().chain(stating.filter(|sentence| {
        !values.is_empty()
            && super::publish::value_tokens(sentence)
                .iter()
                .any(|v| values.contains(v))
    }));
    for sentence in own {
        if let Some(m) = PLACE.find(sentence) {
            if !countries_in(m.as_str()).contains(&true) {
                return Some("offer_place".into());
            }
        }
    }
    let path = Url::parse(page_url)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    if let Some(c) = LOCALE.captures(&path) {
        if !c[1].eq_ignore_ascii_case("kr") && quotes.iter().any(|q| STORE.is_match(q)) {
            return Some(format!("store_locale:{}", &c[1].to_ascii_lowercase()));
        }
    }
    None
}

// ── run ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub candidate_ids: Vec<String>,
    pub url: String,
    pub final_url: Option<String>,
    pub domain: String,
    pub brand: Option<String>,
    pub brand_id: Option<String>,
    pub domain_id: Option<String>,
    /// `proposal` or the rejection reason.
    pub outcome: String,
    pub http_status: Option<u16>,
    pub page_title: Option<String>,
    pub offer: Option<Offer>,
    /// Visible characters of the page read (lane check), when one was read.
    #[serde(skip)]
    pub visible_chars: Option<usize>,
    /// Hash of the visible text of a proposal's page (no body is kept).
    #[serde(skip)]
    pub page_hash: Option<String>,
    /// Visible text of a proposal's page, in memory only for the whole-page
    /// ending scan; never serialized or stored.
    #[serde(skip)]
    pub page_text: Option<String>,
    /// The page's HTML for the whole-page scan, in memory only.
    #[serde(skip)]
    pub page_html: Option<String>,
    /// What the page is read for (the candidate's [`CandidateRow::scope`]).
    #[serde(skip)]
    pub scope: Scope,
}

#[derive(Debug, Clone, Serialize)]
pub struct DomainReport {
    pub domain: String,
    pub robots: String,
    pub tos: &'static str,
    #[serde(skip)]
    pub tos_signal: TosSignal,
    pub terms_url: Option<String>,
    pub requests: u32,
    pub stopped: Option<&'static str>,
    pub crawl_delay_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapReport {
    pub today: NaiveDate,
    pub user_agent: String,
    pub funnel: BTreeMap<String, usize>,
    pub domains: Vec<DomainReport>,
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// G1 global switch for this run.
    pub gray_generic_tos: bool,
    /// G2: robots 404 means allow-all.
    pub gray_robots_missing: bool,
    pub concurrency: usize,
    /// Tests only: spacing below the 10 s floor for loopback servers.
    pub test_spacing: Option<std::time::Duration>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gray_generic_tos: true,
            gray_robots_missing: true,
            concurrency: 8,
            test_spacing: None,
        }
    }
}

fn reason_for_state(state: &PageState, fetched: &Fetched) -> &'static str {
    match state {
        PageState::Disallowed => "robots_disallow",
        PageState::BotWall => "bot_wall",
        PageState::RateLimited(_) => "rate_limited",
        PageState::Gone => "page_gone",
        PageState::Transient => "page_unreachable",
        PageState::Unsupported => "unsupported_content",
        PageState::Redirect {
            same_host: false, ..
        } => "redirect_off_host",
        PageState::Redirect { .. } => "redirect_loop",
        PageState::Html { .. } => "html",
        PageState::NotProbed => match fetched {
            Fetched::Stopped("robots_forbidden") => "robots_forbidden",
            Fetched::Stopped("robots_challenge") => "robots_challenge",
            Fetched::Stopped("robots_html") => "robots_html",
            Fetched::Stopped("robots_not_text") => "robots_not_text",
            Fetched::Stopped("opt_out") => "opted_out",
            Fetched::Stopped("page_bot_wall") => "domain_stopped_bot_wall",
            Fetched::Stopped("rate_limited") => "domain_stopped_rate_limited",
            // The domain is paused (a wall or 401/403 to our agent earlier,
            // or this run stopped on one): a policy state, not a fault.
            Fetched::Stopped("domain_paused" | "deferred") => "domain_paused",
            Fetched::Stopped("domain_stopped") => "domain_stopped",
            // robots `Visit-time` closed now: retried later, never a decision.
            Fetched::Stopped("visit_window") => "visit_window",
            // robots.txt redirects off the host or in a loop: named, a policy
            // state (the page is never requested), not "unavailable".
            Fetched::Stopped("robots_redirect_off_host") => "robots_redirect_off_host",
            Fetched::Stopped("robots_redirect_loop") => "robots_redirect_loop",
            Fetched::Stopped(_) => "robots_unavailable",
            Fetched::BudgetExhausted => "request_budget",
            Fetched::Failed(_) => "page_unreachable",
            Fetched::Reply(_) => "unexpected_reply",
        },
    }
}

/// Fetches a page through [`policy::fetch_resolving_same_host`]. Returns the
/// final URL, the outcome reason (or `html`), the status and the decoded
/// HTML. With `opted_out`, every URL (the page, a redirect hop) is checked
/// against publisher opt-outs right before it is requested.
async fn fetch_page(
    session: &mut DomainSession<'_>,
    url: &Url,
    gray_robots_missing: bool,
    opted_out: Option<&dyn OptOutCheck>,
) -> (Url, &'static str, Option<u16>, Option<String>) {
    if let Some(check) = opted_out {
        if check.covers(url).await {
            return (url.clone(), "opted_out", None, None);
        }
    }
    let policy::Resolved {
        fetched,
        state,
        url: current,
    } = policy::fetch_resolving_same_host(session, url, gray_robots_missing, opted_out).await;
    let status = match &fetched {
        Fetched::Reply(reply) => Some(reply.status),
        _ => None,
    };
    if let (PageState::Html { .. }, Fetched::Reply(reply)) = (&state, &fetched) {
        return match crate::sources::benefit_pages::decode_page(&reply.body, &reply.content_type) {
            Ok(html) => (current, "html", status, Some(html)),
            Err(_) => (current, "unsupported_content", status, None),
        };
    }
    (current, reason_for_state(&state, &fetched), status, None)
}

/// The request-free half of a decision: the deterministic extractor on a
/// page already read under policy, every quote verbatim visible text of it.
/// Sets `outcome` to `proposal` or the rejection reason.
pub(crate) fn read_page(decision: &mut Decision, html: &str, today: NaiveDate) {
    decision.page_title = page_title(html).or(decision.page_title.take());
    match extract_offer_scoped(html, today, &decision.scope) {
        Ok(offer) => {
            // Every quote must be verbatim visible text of this page.
            let page = PageEvidence::from_html(html);
            let all_present = offer
                .facts
                .iter()
                .chain(offer.verification.iter())
                .all(|f| page.contains(&f.quote));
            let text = visible_text(html);
            decision.visible_chars = Some(text.chars().count());
            if all_present {
                decision.outcome = "proposal".into();
                decision.offer = Some(offer);
                decision.page_hash = Some(page.hash());
                decision.page_text = Some(text);
                decision.page_html = Some(html.to_string());
            } else {
                decision.outcome = "quote_not_verbatim".into();
            }
        }
        Err(reason) => decision.outcome = reason.into(),
    }
}

/// One domain: policy, robots, terms, pages, extraction.
pub async fn run_domain(
    session: &mut DomainSession<'_>,
    rows: &[CandidateRow],
    settings: &Settings,
    today: NaiveDate,
) -> (DomainReport, Vec<Decision>) {
    run_domain_with(session, rows, settings, today, precheck, None).await
}

/// Tests only: loopback servers speak plain http on a random port, which
/// [`precheck`] (rightly) refuses.
#[cfg(test)]
pub(crate) async fn run_domain_unchecked(
    session: &mut DomainSession<'_>,
    rows: &[CandidateRow],
    settings: &Settings,
    today: NaiveDate,
) -> (DomainReport, Vec<Decision>) {
    run_domain_with(
        session,
        rows,
        settings,
        today,
        |row| Url::parse(&row.url).map_err(|_| "invalid_url"),
        None,
    )
    .await
}

/// `check` is the request-free policy ([`precheck`] in production);
/// `opted_out`, when given, is asked before every page and terms request.
pub(crate) async fn run_domain_with(
    session: &mut DomainSession<'_>,
    rows: &[CandidateRow],
    settings: &Settings,
    today: NaiveDate,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    opted_out: Option<&dyn OptOutCheck>,
) -> (DomainReport, Vec<Decision>) {
    let domain = rows.first().map(|r| r.domain.clone()).unwrap_or_default();
    // Identical URLs are fetched once; every candidate id is kept.
    let mut by_url: Vec<(String, Vec<&CandidateRow>)> = Vec::new();
    for row in rows {
        let key = row.url.trim().to_string();
        match by_url.iter_mut().find(|(u, _)| *u == key) {
            Some((_, list)) => list.push(row),
            None => by_url.push((key, vec![row])),
        }
    }
    let mut tos: Option<TosSignal> = None;
    let mut terms_url: Option<String> = None;
    let mut robots_label = "not_checked".to_string();
    let mut crawl_delay = None;
    let mut decisions = Vec::new();
    for (url_text, group) in &by_url {
        let first = group[0];
        let mut decision = Decision {
            candidate_ids: group.iter().map(|r| r.id.clone()).collect(),
            url: url_text.clone(),
            final_url: None,
            domain: first.domain.clone(),
            brand: first.brand_name.clone(),
            brand_id: first.brand_id.clone(),
            domain_id: first.domain_id.clone(),
            outcome: String::new(),
            http_status: None,
            page_title: first.page_title.clone(),
            offer: None,
            visible_chars: None,
            page_hash: None,
            page_text: None,
            page_html: None,
            scope: first.scope.clone(),
        };
        let url = match check(first) {
            Ok(url) => url,
            Err(reason) => {
                decision.outcome = reason.into();
                decisions.push(decision);
                continue;
            }
        };
        if tos == Some(TosSignal::SpecificCollection) {
            decision.outcome = "tos_specific_collection".into();
            decisions.push(decision);
            continue;
        }
        if let Some(check) = opted_out {
            // Before any request to the host, robots included.
            if check.covers(&url).await {
                decision.outcome = "opted_out".into();
                decisions.push(decision);
                continue;
            }
        }
        let robots = session.robots(&url).await;
        robots_label = robots.label().to_string();
        crawl_delay = robots.crawl_delay();
        let (final_url, reason, status, html) =
            fetch_page(session, &url, settings.gray_robots_missing, opted_out).await;
        decision.http_status = status;
        if final_url != url {
            decision.final_url = Some(final_url.to_string());
        }
        let Some(html) = html else {
            decision.outcome = reason.into();
            decisions.push(decision);
            continue;
        };
        if tos.is_none() {
            let signal = match find_terms_link(&html, &final_url) {
                None => TosSignal::NotFound,
                Some(terms) => {
                    terms_url = Some(terms.to_string());
                    match fetch_page(session, &terms, settings.gray_robots_missing, opted_out).await
                    {
                        (_, "html", _, Some(page)) => classify_tos(&visible_text(&page)),
                        _ => TosSignal::Unfetchable,
                    }
                }
            };
            tos = Some(signal);
        }
        match tos {
            Some(TosSignal::SpecificCollection) => {
                decision.outcome = "tos_specific_collection".into();
                decisions.push(decision);
                continue;
            }
            Some(TosSignal::GenericAutomated) if !settings.gray_generic_tos => {
                decision.outcome = "tos_generic_gray_off".into();
                decisions.push(decision);
                continue;
            }
            _ => {}
        }
        read_page(&mut decision, &html, today);
        decisions.push(decision);
    }
    (
        DomainReport {
            domain,
            robots: robots_label,
            tos: tos.unwrap_or(TosSignal::Unknown).as_str(),
            tos_signal: tos.unwrap_or(TosSignal::Unknown),
            terms_url,
            requests: session.requests(),
            stopped: session.stopped(),
            crawl_delay_secs: crawl_delay,
        },
        decisions,
    )
}

/// Runs every domain, domains in parallel, one session per domain.
pub async fn run(
    http: &dyn PageHttp,
    rows: Vec<CandidateRow>,
    settings: Settings,
    today: NaiveDate,
) -> Result<BootstrapReport> {
    use futures_util::stream::{self, StreamExt};
    if !http.user_agent().starts_with("MazimiBenefitBot/") {
        bail!("the bootstrap only sends the MazimiBenefitBot user agent");
    }
    let mut by_domain: BTreeMap<String, Vec<CandidateRow>> = BTreeMap::new();
    for row in rows {
        by_domain
            .entry(row.domain.trim().to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    let settings_ref = &settings;
    let jobs = by_domain.into_values().map(|rows| async move {
        let mut session = DomainSession::new(http, None, None, true, 0, MAX_REQUESTS_PER_DOMAIN);
        if let Some(spacing) = settings_ref.test_spacing {
            session = session.with_test_spacing(spacing);
        }
        run_domain(&mut session, &rows, settings_ref, today).await
    });
    let results: Vec<(DomainReport, Vec<Decision>)> = stream::iter(jobs)
        .buffer_unordered(settings.concurrency.clamp(1, 16))
        .collect()
        .await;
    let mut funnel: BTreeMap<String, usize> = BTreeMap::new();
    let mut domains = Vec::new();
    let mut decisions = Vec::new();
    for (report, list) in results {
        domains.push(report);
        for decision in list {
            *funnel.entry(decision.outcome.clone()).or_default() += decision.candidate_ids.len();
            decisions.push(decision);
        }
    }
    domains.sort_by(|a, b| a.domain.cmp(&b.domain));
    decisions.sort_by(|a, b| (&a.domain, &a.url).cmp(&(&b.domain, &b.url)));
    Ok(BootstrapReport {
        today,
        user_agent: http.user_agent().to_string(),
        funnel,
        domains,
        decisions,
    })
}

/// CLI input gate (review M12): every row carries the policy fields
/// [`precheck`] reads. A missing one would read as verified, approved and
/// active, so an export without them is refused before any request.
pub fn check_input(rows: &[CandidateRow]) -> Result<()> {
    let incomplete: Vec<&str> = rows
        .iter()
        .filter(|r| {
            r.verification_status.is_none()
                || r.compliance_status.is_none()
                || r.domain_active.is_none()
        })
        .map(|r| r.id.as_str())
        .collect();
    if !incomplete.is_empty() {
        bail!(
            "{} candidate rows lack verification_status, compliance_status or domain_active (first: {})",
            incomplete.len(),
            incomplete[..incomplete.len().min(5)].join(", ")
        );
    }
    Ok(())
}

/// Outcomes that are a decision about a candidate (policy, robots, terms,
/// extraction), not a failure to read it. Allowlisted (review L-2): any
/// other outcome (unreachable page or robots, rate limit, unexpected
/// reply, or one added later) makes the run incomplete (review M12).
const DECISIONS: &[&str] = &[
    "proposal",
    "quote_not_verbatim",
    // precheck
    "campus_out_of_scope",
    "domain_not_verified",
    "domain_compliance_rejected",
    "prior_review_terms_restrict",
    "invalid_url",
    "not_https",
    "archive_or_aggregator",
    "off_brand_domain",
    "third_party_content",
    // policy before and while fetching
    "opted_out",
    "tos_specific_collection",
    "tos_generic_gray_off",
    "robots_disallow",
    "robots_forbidden",
    "robots_challenge",
    "robots_html",
    "robots_not_text",
    "bot_wall",
    "domain_stopped_bot_wall",
    "domain_paused",
    "domain_stopped",
    "robots_redirect_off_host",
    "robots_redirect_loop",
    "request_budget",
    "page_gone",
    "unsupported_content",
    "redirect_loop",
    "redirect_off_host",
    // extraction
    "js_shell",
    "offer_ended",
    "product_listing",
    "no_student_youth_offer",
    "multi_offer_page",
    "conflicting_ages",
    "stale_page",
    "conflicting_terms",
    "short_free_term",
    "unbound_term",
    "template_shell",
];

/// Writes the report only for a complete run, and atomically (a temporary
/// file in the same directory, then a rename): a failed run exits non-zero
/// and leaves no file, or an earlier one, untouched (review M12).
pub fn write_report(report: &BootstrapReport, out: &std::path::Path) -> Result<()> {
    let failed: Vec<&Decision> = report
        .decisions
        .iter()
        .filter(|d| !DECISIONS.contains(&d.outcome.as_str()))
        .collect();
    if !failed.is_empty() {
        let sample: Vec<String> = failed
            .iter()
            .take(5)
            .map(|d| format!("{} {}", d.outcome, d.url))
            .collect();
        bail!(
            "{} pages could not be read; nothing written ({})",
            failed.len(),
            sample.join("; ")
        );
    }
    let body = serde_json::to_string_pretty(report)?;
    let name = out
        .file_name()
        .context("--out names a file")?
        .to_string_lossy();
    let temporary = out.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, body)
        .and_then(|()| std::fs::rename(&temporary, out))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
        .with_context(|| format!("failed to write {}", out.display()))
}

/// The bot identity for a run; the contact URL must be public https.
pub fn bootstrap_user_agent(contact_url: &str) -> Result<String> {
    super::validate_contact_url(contact_url)?;
    Ok(super::user_agent(contact_url))
}

/// Groups decisions by reason for the summary printout.
pub fn funnel_text(report: &BootstrapReport) -> String {
    let mut out = String::new();
    let total: usize = report.funnel.values().sum();
    out.push_str(&format!("candidates: {total}\n"));
    let mut sorted: Vec<_> = report.funnel.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, count) in sorted {
        out.push_str(&format!("  {reason:<32} {count}\n"));
    }
    let mut by_domain: HashMap<&str, usize> = HashMap::new();
    for d in report.decisions.iter().filter(|d| d.outcome == "proposal") {
        *by_domain.entry(d.domain.as_str()).or_default() += 1;
    }
    out.push_str(&format!("proposal domains: {}\n", by_domain.len()));
    out
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
