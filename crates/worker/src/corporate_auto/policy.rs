//! Collection policy shared by the census and the evidence job: robots per
//! agent and path, challenge detection, the ToS signal, the lane chooser and
//! one polite request session per domain.
//!
//! Hard rules enforced here (design §2.2, ABSOLUTE rows):
//! - A1: a 401, 403, 429 or challenge page stops every further request to
//!   the domain for the rest of the run. Nothing is retried with another
//!   client or user agent.
//! - A3: robots.txt is evaluated for `MazimiBenefitBot` on the exact path
//!   and query before every page request. A robots file served as HTML, as a
//!   challenge, or behind 401/403 fails closed.
//! - A4: one request in flight per domain, spaced by
//!   `max(10 s, Crawl-delay, domain interval)` (30 s floor when the domain's
//!   G3 switch is off), plus a per-run request cap.

use anyhow::Result;
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::Serialize;
use sqlx::PgPool;
use std::{collections::HashMap, sync::LazyLock, time::Duration};
use tokio::time::Instant;
use uuid::Uuid;

use super::http::{FetchError, HttpReply, PageHttp, MAX_PAGE_BYTES, MAX_ROBOTS_BYTES};
use super::{MIN_REQUEST_INTERVAL, STRICT_REQUEST_INTERVAL};
use crate::sources::benefit_pages::{
    robots_allows, robots_crawl_delay, robots_response_looks_like_html, robots_visit_time,
    BOT_PRODUCT_TOKEN,
};

/// Below this many visible characters an HTML page is treated as a script
/// shell that needs the render lane (design §3.3).
pub const JS_SHELL_CHARS: usize = 500;
/// After 401/403 or a challenge the domain is left alone this long (design
/// §3.2.5: retry once after 7 days, never with another client).
pub const BOT_WALL_PAUSE_DAYS: i64 = 7;

/// Which source domains the census and the evidence job may touch at all,
/// for a `benefit_source_domains` row aliased `d`. A domain a review
/// rejected, robots blocked, ToS restricted, a publisher blocked, or whose
/// approval a person withdrew (`approved` but not crawlable) is out, and so
/// is any domain that serves no corporate or campus benefit.
/// The domain checks every reader applies, for a `benefit_source_domains`
/// row aliased `d`: active, verified, no rejected review, not blocked by
/// collection mode, robots or a terms review, and either approved for
/// crawling or still pending.
macro_rules! domain_policy_sql {
    () => {
        "d.is_active AND d.verification_status = 'verified'
    AND d.compliance_status <> 'rejected'
    AND d.collection_mode <> 'blocked'
    AND d.robots_status <> 'blocked'
    AND COALESCE(d.terms_review_outcome, '') <> 'restricted'
    AND (d.crawl_allowed OR d.compliance_status IN ('pending', 'expired'))"
    };
}

/// A candidate the door job alone reads (security review H4, code review
/// B2), for a candidate row aliased `{c}`: the list page an issuer hub's
/// door owns, or any candidate a `door`-mode monitor watches. Every other
/// reader (census, the hash fetcher, fresh extraction, the weekly audit,
/// intake, discovery) leaves it alone, so the hub is read only under the
/// door switches and its terms gate, at most once a day.
const HUB_OWNED_CANDIDATE_SQL: &str = "(EXISTS (SELECT 1 FROM corporate_doors hub_door
         WHERE hub_door.candidate_id = {c}.id AND hub_door.kind = 'hub_rows')
     OR EXISTS (SELECT 1 FROM program_source_monitors hub_monitor
         WHERE hub_monitor.candidate_id = {c}.id AND hub_monitor.verification_mode = 'door'))";

/// [`HUB_OWNED_CANDIDATE_SQL`] for the candidate alias `c`.
pub fn hub_owned(c: &str) -> String {
    HUB_OWNED_CANDIDATE_SQL.replace("{c}", c)
}

/// [`ELIGIBLE_DOMAIN_SQL`]'s domain checks alone (the door job's: a door
/// is its own reason to read the domain).
pub const DOMAIN_POLICY_SQL: &str = concat!("(", domain_policy_sql!(), ")");

/// Which source domains the census and the evidence job may touch at all,
/// for a `benefit_source_domains` row aliased `d`. A domain a review
/// rejected, robots blocked, ToS restricted, a publisher blocked, or whose
/// approval a person withdrew (`approved` but not crawlable) is out, and so
/// is any domain that serves no corporate or campus benefit. An issuer
/// hub's list page is excluded by its candidate ([`hub_owned`]), never by
/// its domain: the other pages of `card.kbcard.com` stay readable.
pub const ELIGIBLE_DOMAIN_SQL: &str = concat!(
    "(",
    domain_policy_sql!(),
    "
    AND (
        EXISTS (SELECT 1 FROM benefit_source_candidates ec
                JOIN program_source_monitors em ON em.candidate_id = ec.id AND em.verification_state <> 'retired'
                JOIN programs ep ON ep.id = em.program_id
                WHERE ec.domain_id = d.id AND ep.program_type IN ('corporate_benefit', 'campus_benefit'))
        OR EXISTS (SELECT 1 FROM benefit_source_candidates ec
                   JOIN benefit_candidate_brands eb ON eb.candidate_id = ec.id
                   WHERE ec.domain_id = d.id)
    )
)"
);

/// The longest a domain-wide pause may park a run before the domain is
/// skipped for this run instead.
pub(crate) const MAX_SLOT_WAIT: Duration = Duration::from_secs(10 * 60);
/// Same-host robots.txt redirects followed (RFC 9309 §2.3.1.2: at least 5).
const MAX_ROBOTS_REDIRECTS: u32 = 5;

// ── robots ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum RobotsState {
    Ok {
        body: String,
        crawl_delay: Option<f64>,
    },
    /// 404/410: RFC 9309 treats a missing file as allow-all (G2).
    Missing,
    /// 401/403, challenge, HTML or non-text body: fail closed.
    Blocked(&'static str),
    RateLimited(DateTime<Utc>),
    /// 5xx, timeouts, TLS: try again on a later run.
    Unavailable(&'static str),
}

impl RobotsState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::Missing => "missing",
            Self::Blocked(reason) => reason,
            Self::RateLimited(_) => "rate_limited",
            Self::Unavailable(reason) => reason,
        }
    }

    /// Whether `target` (path plus query) may be fetched by our agent, by
    /// the decision every fetch path shares ([`robots_allows`]). `None`
    /// when robots could not be read at all.
    pub fn allows(&self, target: &str, gray_robots_missing: bool) -> Option<bool> {
        match self {
            Self::Ok { body, .. } => Some(robots_allows(body, BOT_PRODUCT_TOKEN, target)),
            Self::Missing => Some(gray_robots_missing),
            _ => None,
        }
    }

    /// The root is closed to us while a search engine is let in: the
    /// allowlist pattern that only the search-snippet lane can serve.
    pub fn search_engine_allowlist(&self) -> bool {
        match self {
            Self::Ok { body, .. } => {
                self.allows("/", false) == Some(false)
                    && ["Googlebot", "Yeti", "Daum", "bingbot"]
                        .iter()
                        .any(|agent| robots_allows(body, agent, "/"))
            }
            _ => false,
        }
    }

    pub fn crawl_delay(&self) -> Option<f64> {
        match self {
            Self::Ok { crawl_delay, .. } => *crawl_delay,
            _ => None,
        }
    }

    /// Whether robots `Visit-time` lets our agent request now (UTC). A file
    /// that states no window for our agent allows any time.
    pub fn visit_allowed(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Ok { body, .. } => {
                let minute = (now.timestamp().rem_euclid(86_400) / 60) as u16;
                robots_visit_time(body, BOT_PRODUCT_TOKEN).allows(minute)
            }
            _ => true,
        }
    }
}

pub fn classify_robots(reply: &HttpReply) -> RobotsState {
    match reply.status {
        404 | 410 => return RobotsState::Missing,
        401 | 403 => return RobotsState::Blocked("robots_forbidden"),
        429 => return RobotsState::RateLimited(reply.retry_at),
        200 => {}
        _ if looks_like_challenge(reply.status, &reply.body) => {
            return RobotsState::Blocked("robots_challenge")
        }
        _ => return RobotsState::Unavailable("robots_http_error"),
    }
    if looks_like_challenge(reply.status, &reply.body) {
        return RobotsState::Blocked("robots_challenge");
    }
    if robots_response_looks_like_html(&reply.body) {
        return RobotsState::Blocked("robots_html");
    }
    let content_type = reply.content_type.as_str();
    if !content_type.is_empty()
        && !content_type.starts_with("text/plain")
        && !content_type.starts_with("application/octet-stream")
    {
        return RobotsState::Blocked("robots_not_text");
    }
    let body = String::from_utf8_lossy(&reply.body).into_owned();
    let crawl_delay = robots_crawl_delay(&body, BOT_PRODUCT_TOKEN);
    RobotsState::Ok { body, crawl_delay }
}

/// Request spacing for one domain: never below 10 s (30 s when the domain's
/// G3 switch is off), never below the robots Crawl-delay or the domain's
/// own configured interval.
pub fn effective_interval(
    crawl_delay_secs: Option<f64>,
    gray_crawl_delay_min: bool,
    domain_interval_ms: i64,
) -> Duration {
    let floor = if gray_crawl_delay_min {
        MIN_REQUEST_INTERVAL
    } else {
        STRICT_REQUEST_INTERVAL
    };
    let crawl = crawl_delay_secs
        .filter(|delay| delay.is_finite() && *delay >= 0.0)
        .map(|delay| Duration::from_secs_f64(delay.min(3_600.0)))
        .unwrap_or_default();
    let domain = Duration::from_millis(domain_interval_ms.clamp(0, 600_000) as u64);
    floor.max(crawl).max(domain)
}

// ── pages ─────────────────────────────────────────────────────────────────

static CHALLENGE_MARKERS: &[&str] = &[
    "cf-chl",
    "challenge-platform",
    "just a moment...",
    "enable javascript and cookies to continue",
    "attention required! | cloudflare",
    "captcha-delivery",
    "_incapsula_resource",
    "px-captcha",
    "g-recaptcha",
    "hcaptcha",
];

/// Bot-wall and challenge pages. Any of these is treated like a 403 (A1).
pub fn looks_like_challenge(status: u16, body: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&body[..body.len().min(64 * 1024)]).to_ascii_lowercase();
    if CHALLENGE_MARKERS
        .iter()
        .any(|marker| prefix.contains(marker))
    {
        return true;
    }
    // Akamai and similar edge denials answer 403/503 with a short page.
    matches!(status, 403 | 503)
        && (prefix.contains("access denied") || prefix.contains("request blocked"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageState {
    NotProbed,
    Html {
        text_chars: usize,
    },
    Redirect {
        location: String,
        same_host: bool,
    },
    BotWall,
    RateLimited(DateTime<Utc>),
    Gone,
    Transient,
    Unsupported,
    /// Our own robots evaluation forbade the request.
    Disallowed,
}

pub fn classify_page(reply: &HttpReply, page_url: &Url) -> PageState {
    match reply.status {
        200 => {
            if looks_like_challenge(200, &reply.body) {
                return PageState::BotWall;
            }
            if !reply.content_type.starts_with("text/html")
                && !reply.content_type.starts_with("application/xhtml+xml")
            {
                return PageState::Unsupported;
            }
            let html = String::from_utf8_lossy(&reply.body);
            PageState::Html {
                text_chars: super::evidence::visible_text(&html).chars().count(),
            }
        }
        301 | 302 | 303 | 307 | 308 => {
            let location = reply.location.clone().unwrap_or_default();
            let same_host = page_url.join(&location).ok().is_some_and(|next| {
                next.host_str() == page_url.host_str() && next.scheme() == page_url.scheme()
            });
            PageState::Redirect {
                location,
                same_host,
            }
        }
        401 | 403 => PageState::BotWall,
        429 => PageState::RateLimited(reply.retry_at),
        404 | 410 => PageState::Gone,
        status if looks_like_challenge(status, &reply.body) => PageState::BotWall,
        500..=599 => PageState::Transient,
        _ => PageState::Unsupported,
    }
}

/// Path plus query, the exact target robots rules are evaluated against.
pub fn robots_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    }
}

// ── ToS signal (design §3.2.3) ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TosSignal {
    None,
    LoginOnly,
    GenericAutomated,
    SpecificCollection,
    Unfetchable,
    NotFound,
    Unknown,
}

impl TosSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LoginOnly => "login_only",
            Self::GenericAutomated => "generic_automated",
            Self::SpecificCollection => "specific_collection",
            Self::Unfetchable => "unfetchable",
            Self::NotFound => "not_found",
            Self::Unknown => "unknown",
        }
    }
}

static TOS_CLAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(자동화|프로그램|로봇|스파이더|크롤|스크랩|스크래핑|매크로|automated|\bbots?\b|crawl|scrap|spider)[^.。!?\n]{0,80}?(금지|제한|사전[^.。\n]{0,10}(동의|승낙|허락)|prohibit|not permitted|forbidden|may not)|(prohibit|forbid|may not|must not|not permitted to)[^.。!?\n]{0,80}?(automated|\bbots?\b|crawl|scrap|spider|robot)",
    )
    .expect("valid ToS clause pattern")
});
static TOS_LOGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(로그인|계정|회원\s*가입|login|log in|sign[- ]?in|account)").expect("valid")
});
static TOS_SPECIFIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(수집|복제|복사|게시|배포|재배포|가공|재가공|republish|redistribut|reproduc|harvest)[^.。\n]{0,60}(제3자|타인|상업|영리|third[- ]part|commercial|가격|이벤트|price|event)|(제3자|타인|상업|영리|third[- ]part|commercial)[^.。\n]{0,60}(수집|복제|게시|배포|재배포|republish|redistribut)",
    )
    .expect("valid")
});

/// Deterministic classification of a terms page's text. Only the matched
/// clause decides: `login_only` when it is about automated log-in,
/// `specific_collection` when it names collecting or republishing site
/// information for others (blocks the domain), otherwise
/// `generic_automated` (allowed only while G1 is on).
pub fn classify_tos(text: &str) -> TosSignal {
    let mut signal = TosSignal::None;
    for clause in TOS_CLAUSE.find_iter(text) {
        // The sentence around the clause carries the object of the ban.
        let start = text[..clause.start()]
            .rfind(['.', '。', '\n'])
            .map_or(0, |i| i + 1);
        let end = text[clause.end()..]
            .find(['.', '。', '\n'])
            .map_or(text.len(), |i| clause.end() + i);
        let sentence = &text[start..end];
        let this = if TOS_SPECIFIC.is_match(sentence) {
            TosSignal::SpecificCollection
        } else if TOS_LOGIN.is_match(sentence) {
            TosSignal::LoginOnly
        } else {
            TosSignal::GenericAutomated
        };
        signal = strongest(signal, this);
    }
    signal
}

fn strongest(a: TosSignal, b: TosSignal) -> TosSignal {
    let rank = |s: TosSignal| match s {
        TosSignal::SpecificCollection => 3,
        TosSignal::GenericAutomated => 2,
        TosSignal::LoginOnly => 1,
        _ => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// The footer link to the terms page, on the same host only.
pub fn find_terms_link(html: &str, page_url: &Url) -> Option<Url> {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*(서비스\s*)?이용\s*약관\s*$|^\s*terms(\s+of\s+(service|use))?\s*$|^\s*약관\s*$")
            .expect("valid")
    });
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").ok()?;
    document.select(&selector).find_map(|anchor| {
        let label = anchor.text().collect::<Vec<_>>().join(" ");
        if !LABEL.is_match(label.trim()) {
            return None;
        }
        let url = page_url.join(anchor.value().attr("href")?.trim()).ok()?;
        (url.host_str() == page_url.host_str() && url.scheme() == page_url.scheme()).then_some(url)
    })
}

// ── lanes ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Direct,
    Render,
    Secondary,
    Snippet,
    Feed,
    Impossible,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Render => "render",
            Self::Secondary => "secondary",
            Self::Snippet => "snippet",
            Self::Feed => "feed",
            Self::Impossible => "impossible",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaneInput<'a> {
    pub opted_out: bool,
    pub feed_active: bool,
    pub robots: &'a RobotsState,
    pub path_allowed: Option<bool>,
    pub page: &'a PageState,
    pub tos: TosSignal,
    /// Global G1 flag AND the domain's switch.
    pub gray_generic_tos: bool,
    pub gray_robots_missing: bool,
}

macro_rules! lane_reasons {
    ($($variant:ident => $text:literal),* $(,)?) => {
        /// Why [`choose_lane`] put a URL in its lane. One list: the census
        /// writes these as `corporate_census_entries.reason`, and every
        /// reader that rates a page itself uses the same rule.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum LaneReason {
            $($variant),*
        }

        impl LaneReason {
            /// Every reason, generated from the same list as the enum.
            pub const ALL: &'static [LaneReason] = &[$(LaneReason::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(LaneReason::$variant => $text),*
                }
            }
        }
    };
}

lane_reasons! {
    OptOut => "opt_out",
    PartnerFeed => "partner_feed",
    RobotsBotWall => "robots_bot_wall",
    RateLimited => "rate_limited",
    DomainPaused => "domain_paused",
    RobotsUnavailable => "robots_unavailable",
    RobotsMissingGrayOff => "robots_missing_gray_off",
    RobotsDisallow => "robots_disallow",
    TosSpecificCollection => "tos_specific_collection",
    TosGenericGrayOff => "tos_generic_gray_off",
    RobotsAllowedUnprobed => "robots_allowed_unprobed",
    JsShell => "js_shell",
    DirectHtml => "direct_html",
    RedirectLoop => "redirect_loop",
    RedirectOffHost => "redirect_off_host",
    PageBotWall => "page_bot_wall",
    PageGone => "page_gone",
    PageUnreachable => "page_unreachable",
    UnsupportedContent => "unsupported_content",
}

impl LaneReason {
    /// Whether a rating with this reason says something about the URL.
    /// A reason our own back-off or a passing reply produced (our pause on
    /// the domain, a 429, robots.txt we could not reach, a 5xx or a
    /// timeout, a page never fetched) is no verdict: it never replaces the
    /// last real rating (production 2026-11-07: the census itself got a 429
    /// from support.apple.com and its row replaced Apple Music's
    /// `direct_html`). A robots disallow, a wall, a script shell, a gone
    /// page, an opt-out, a terms clause are verdicts. Exhaustive: a reason
    /// added later is classified here before it compiles.
    pub fn is_verdict(self) -> bool {
        match self {
            Self::DomainPaused
            | Self::RateLimited
            | Self::RobotsUnavailable
            | Self::PageUnreachable
            | Self::RobotsAllowedUnprobed => false,
            Self::OptOut
            | Self::PartnerFeed
            | Self::RobotsBotWall
            | Self::RobotsMissingGrayOff
            | Self::RobotsDisallow
            | Self::TosSpecificCollection
            | Self::TosGenericGrayOff
            | Self::JsShell
            | Self::DirectHtml
            | Self::RedirectLoop
            | Self::RedirectOffHost
            | Self::PageBotWall
            | Self::PageGone
            | Self::UnsupportedContent => true,
        }
    }
}

/// Lane for one URL (design §3.3), first matching rule wins.
pub fn choose_lane(input: &LaneInput<'_>) -> (Lane, &'static str) {
    let (lane, reason) = rate_lane(input);
    (lane, reason.as_str())
}

/// The lane a terms signal alone refuses, as [`rate_lane`] rates it.
fn tos_refusal(tos: TosSignal, gray_generic_tos: bool) -> Option<(Lane, LaneReason)> {
    match tos {
        TosSignal::SpecificCollection => {
            Some((Lane::Impossible, LaneReason::TosSpecificCollection))
        }
        TosSignal::GenericAutomated if !gray_generic_tos => {
            Some((Lane::Impossible, LaneReason::TosGenericGrayOff))
        }
        _ => None,
    }
}

/// What [`rate_lane`] refuses from facts known before any request: an
/// active partner feed, or a stored terms signal it refuses (review A-H2:
/// a reader with no census rating must not fetch a page the census would
/// never have requested).
pub fn refused_without_request(
    feed_active: bool,
    stored_tos: Option<TosSignal>,
    gray_generic_tos: bool,
) -> Option<(Lane, LaneReason)> {
    if feed_active {
        return Some((Lane::Feed, LaneReason::PartnerFeed));
    }
    stored_tos.and_then(|tos| tos_refusal(tos, gray_generic_tos))
}

/// [`choose_lane`] with its typed reason.
pub fn rate_lane(input: &LaneInput<'_>) -> (Lane, LaneReason) {
    use LaneReason as R;
    if input.opted_out {
        return (Lane::Impossible, R::OptOut);
    }
    if input.feed_active {
        return (Lane::Feed, R::PartnerFeed);
    }
    match input.robots {
        RobotsState::Blocked(_) => return (Lane::Secondary, R::RobotsBotWall),
        RobotsState::RateLimited(_) => return (Lane::Impossible, R::RateLimited),
        // Skipped inside an earlier pause (A1), not a robots fault.
        RobotsState::Unavailable("domain_paused") => return (Lane::Impossible, R::DomainPaused),
        RobotsState::Unavailable(_) => return (Lane::Impossible, R::RobotsUnavailable),
        RobotsState::Missing if !input.gray_robots_missing => {
            return (Lane::Impossible, R::RobotsMissingGrayOff)
        }
        _ => {}
    }
    if input.path_allowed == Some(false) {
        return (Lane::Snippet, R::RobotsDisallow);
    }
    if let Some(refused) = tos_refusal(input.tos, input.gray_generic_tos) {
        return refused;
    }
    match input.page {
        PageState::NotProbed => (Lane::Direct, R::RobotsAllowedUnprobed),
        PageState::Html { text_chars } if *text_chars < JS_SHELL_CHARS => {
            (Lane::Render, R::JsShell)
        }
        PageState::Html { .. } => (Lane::Direct, R::DirectHtml),
        // Out of `fetch_resolving_same_host`, a same-host redirect is one it
        // could not resolve.
        PageState::Redirect {
            same_host: true, ..
        } => (Lane::Impossible, R::RedirectLoop),
        PageState::Redirect { .. } => (Lane::Impossible, R::RedirectOffHost),
        PageState::BotWall => (Lane::Secondary, R::PageBotWall),
        PageState::RateLimited(_) => (Lane::Impossible, R::RateLimited),
        PageState::Gone => (Lane::Impossible, R::PageGone),
        PageState::Transient => (Lane::Impossible, R::PageUnreachable),
        PageState::Unsupported => (Lane::Impossible, R::UnsupportedContent),
        PageState::Disallowed => (Lane::Snippet, R::RobotsDisallow),
    }
}

/// The terms-page signal of the domain `page_url` belongs to, read the way
/// the census reads it: the footer link to the terms on the same host, the
/// opt-out checked before the request, the page through the same polite
/// session. No terms link reads `not_found`; a terms page we may not or
/// could not read reads `unfetchable`.
pub async fn terms_signal(
    session: &mut DomainSession<'_>,
    html: &str,
    page_url: &Url,
    gray_robots_missing: bool,
    opted_out: &dyn OptOutCheck,
) -> TosSignal {
    match find_terms_link(html, page_url) {
        None => TosSignal::NotFound,
        Some(terms) if opted_out.covers(&terms).await => TosSignal::Unfetchable,
        Some(terms) => match session.page(&terms, gray_robots_missing).await {
            (Fetched::Reply(reply), PageState::Html { .. }) => {
                let page = String::from_utf8_lossy(&reply.body);
                classify_tos(&super::evidence::visible_text(&page))
            }
            _ => TosSignal::Unfetchable,
        },
    }
}

// ── opt-out ───────────────────────────────────────────────────────────────

/// Any active collection opt-out covering this URL's host or a URL prefix
/// of it. A display-only opt-out hides offers but does not stop fetching.
pub async fn opted_out(pool: &PgPool, url: &Url) -> Result<bool> {
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM benefit_source_optouts
             WHERE lifted_at IS NULL AND effective_at IS NOT NULL
               AND scope IN ('collection', 'both')
               AND (
                   (domain IS NOT NULL AND ($1 = domain OR $1 LIKE '%.' || domain))
                   OR (url_prefix IS NOT NULL AND starts_with($2, url_prefix))
               )
         )",
    )
    .bind(&host)
    .bind(url.as_str())
    .fetch_one(pool)
    .await?)
}

/// Whether a publisher opt-out covers a URL. Production asks the database
/// right before each request, so an opt-out received during a long run
/// stops the very next fetch (design §2.4, A7).
#[async_trait::async_trait]
pub trait OptOutCheck: Sync {
    async fn covers(&self, url: &Url) -> bool;
}

#[async_trait::async_trait]
impl<F: Fn(&Url) -> bool + Sync> OptOutCheck for F {
    async fn covers(&self, url: &Url) -> bool {
        self(url)
    }
}

/// The live opt-out table. A failed lookup counts as opted out (fail closed).
pub struct LiveOptOuts<'a>(pub &'a PgPool);

#[async_trait::async_trait]
impl OptOutCheck for LiveOptOuts<'_> {
    async fn covers(&self, url: &Url) -> bool {
        opted_out(self.0, url).await.unwrap_or(true)
    }
}

// ── same-host redirects ───────────────────────────────────────────────────
//
// Invariant: every collector that rates or reads a page (the census, the
// evidence job, the bootstrap and re-extraction fetch) follows redirects
// through `fetch_resolving_same_host` and nowhere else, so the URL the
// census rates is the page the evidence job reads.
// - Only same-origin hops are followed, at most `MAX_SAME_HOST_HOPS`. An
//   off-host redirect is returned as it is (`redirect_off_host`): no request
//   ever leaves the host, and a moved brand needs its new domain registered.
// - Each hop is a full `DomainSession::page` request: robots for the hop's
//   own path and query, the shared slot and spacing, the stop on 401/403/429
//   or a challenge. A hop robots disallows is `Disallowed` and not fetched.
// - The publisher opt-out is checked before every hop (the caller checks
//   the first URL); a covered hop ends as `Fetched::Stopped("opt_out")`.
// - A `Redirect { same_host: true }` that comes out of the resolver is
//   unresolved: a self-redirect or more hops than the bound. The lane
//   chooser reads it as `redirect_loop`.
// robots.txt redirects are resolved separately in `DomainSession::robots`
// (RFC 9309, same origin only, five hops).

/// Same-host redirects followed before a page is rated or read.
pub const MAX_SAME_HOST_HOPS: usize = 3;

/// The census-contract check for a `corporate_census_entries` row aliased
/// `entry`: the census fetched the URL and rated it direct. Every probed
/// direct reason is one the resolver reads as a page (`direct_html`; older
/// runs also wrote `redirect_same_host` before the census followed hops), so
/// the evidence job never waits a census cycle for a redirecting URL.
pub const CENSUS_DIRECT_SQL: &str = "(entry.lane = 'direct' AND entry.probed)";

/// Each monitored URL's census verdict, as rows aliased `entry` (one per
/// candidate): its latest monitor row in a completed run at most `days`
/// (a bind parameter) old whose reason is a verdict
/// ([`LaneReason::is_verdict`]). A row whose reason is none (our own pause,
/// a 429, robots unreachable, a 5xx, no probe) never replaces the last
/// real rating; with no verdict at all the reader rates the page itself
/// ([`super::evidence`], one rule: [`rate_lane`]). A reason no current
/// rule writes (an older census's) still counts as a verdict.
pub fn census_verdicts_sql(days: &str) -> String {
    format!(
        "SELECT DISTINCT ON (entry.candidate_id) entry.*
         FROM corporate_census_entries entry
         JOIN corporate_census_runs run ON run.id = entry.run_id
         WHERE run.status = 'completed' AND run.finished_at > now() - make_interval(days => {days})
           AND entry.subject_kind = 'monitor' AND entry.reason NOT IN ({no_verdict})
         ORDER BY entry.candidate_id, run.finished_at DESC",
        no_verdict = no_verdict_reasons_sql(),
    )
}

/// The reasons that are no verdict, as an SQL list of literals (code
/// constants of `[a-z_]` only, never input).
fn no_verdict_reasons_sql() -> String {
    LaneReason::ALL
        .iter()
        .filter(|reason| !reason.is_verdict())
        .map(|reason| {
            debug_assert!(reason
                .as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_'));
            format!("'{}'", reason.as_str())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A page fetch after same-host redirects: the final reply and state and the
/// URL they belong to.
#[derive(Debug)]
pub struct Resolved {
    pub fetched: Fetched,
    pub state: PageState,
    pub url: Url,
}

/// Fetches `url`, following same-host redirects under the invariant above.
pub async fn fetch_resolving_same_host(
    session: &mut DomainSession<'_>,
    url: &Url,
    gray_robots_missing: bool,
    opted_out: Option<&dyn OptOutCheck>,
) -> Resolved {
    fetch_resolving_same_host_limited(session, url, gray_robots_missing, opted_out, MAX_PAGE_BYTES)
        .await
}

/// [`fetch_resolving_same_host`] with the body cap of
/// [`DomainSession::page_limited`].
pub async fn fetch_resolving_same_host_limited(
    session: &mut DomainSession<'_>,
    url: &Url,
    gray_robots_missing: bool,
    opted_out: Option<&dyn OptOutCheck>,
    max_bytes: usize,
) -> Resolved {
    let mut current = url.clone();
    let mut hops = 0;
    loop {
        let (fetched, state) = session
            .page_limited(&current, gray_robots_missing, max_bytes)
            .await;
        let PageState::Redirect {
            location,
            same_host: true,
        } = &state
        else {
            return Resolved {
                fetched,
                state,
                url: current,
            };
        };
        let next = match current.join(location) {
            Ok(next) if next != current && hops < MAX_SAME_HOST_HOPS => next,
            _ => {
                return Resolved {
                    fetched,
                    state,
                    url: current,
                }
            }
        };
        if let Some(check) = opted_out {
            if check.covers(&next).await {
                return Resolved {
                    fetched: Fetched::Stopped("opt_out"),
                    state: PageState::NotProbed,
                    url: next,
                };
            }
        }
        hops += 1;
        current = next;
    }
}

// ── one polite session per domain ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    Reply(HttpReply),
    /// The domain was stopped earlier in this run (A1) or deferred.
    Stopped(&'static str),
    /// The per-run request cap for the domain was reached (A4).
    BudgetExhausted,
    Failed(String),
}

pub struct DomainSession<'a> {
    http: &'a dyn PageHttp,
    pool: Option<&'a PgPool>,
    domain_id: Option<Uuid>,
    gray_crawl_delay_min: bool,
    domain_interval_ms: i64,
    max_requests: u32,
    min_floor: Duration,
    requests: u32,
    last_request: Option<Instant>,
    stopped: Option<&'static str>,
    robots: HashMap<String, RobotsState>,
    /// Tests only: the clock robots `Visit-time` is read against.
    clock: Option<DateTime<Utc>>,
}

impl<'a> DomainSession<'a> {
    /// `pool` + `domain_id` share the request slot with every other collector
    /// through `benefit_source_domains.next_request_at`.
    pub fn new(
        http: &'a dyn PageHttp,
        pool: Option<&'a PgPool>,
        domain_id: Option<Uuid>,
        gray_crawl_delay_min: bool,
        domain_interval_ms: i64,
        max_requests: u32,
    ) -> Self {
        Self {
            http,
            pool,
            domain_id,
            gray_crawl_delay_min,
            domain_interval_ms,
            max_requests,
            min_floor: MIN_REQUEST_INTERVAL,
            requests: 0,
            last_request: None,
            stopped: None,
            robots: HashMap::new(),
            clock: None,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.unwrap_or_else(Utc::now)
    }

    /// Tests only: read robots `Visit-time` against this instant.
    #[cfg(test)]
    pub(crate) fn with_test_clock(mut self, now: DateTime<Utc>) -> Self {
        self.clock = Some(now);
        self
    }

    /// Tests only (reached through crate-private test entry points): a
    /// shorter spacing so a mock server run stays fast. Production callers
    /// never pass one, so the 10 s floor holds.
    pub(crate) fn with_test_spacing(mut self, spacing: Duration) -> Self {
        self.min_floor = spacing;
        self.domain_interval_ms = 0;
        self
    }

    pub fn stopped(&self) -> Option<&'static str> {
        self.stopped
    }

    pub fn requests(&self) -> u32 {
        self.requests
    }

    fn origin_key(url: &Url) -> String {
        format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap_or(""),
            url.port_or_known_default().unwrap_or(0)
        )
    }

    fn interval(&self, url: &Url) -> Duration {
        let crawl = self
            .robots
            .get(&Self::origin_key(url))
            .and_then(RobotsState::crawl_delay);
        let production =
            effective_interval(crawl, self.gray_crawl_delay_min, self.domain_interval_ms);
        if self.min_floor < MIN_REQUEST_INTERVAL {
            // Test spacing: keep the crawl-delay ordering, scaled down.
            self.min_floor.max(Duration::from_millis(
                (crawl.unwrap_or(0.0) * 10.0).min(1_000.0) as u64,
            ))
        } else {
            production
        }
    }

    pub async fn robots(&mut self, url: &Url) -> RobotsState {
        let key = Self::origin_key(url);
        if let Some(state) = self.robots.get(&key) {
            return state.clone();
        }
        let mut robots_url = url.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        // RFC 9309 §2.3.1.2: follow up to five redirects. Only on the same
        // origin: an off-host robots file is never fetched (no redirect
        // leaves the host), and the domain fails closed.
        let mut hops = 0;
        let state = loop {
            match self
                .request(&robots_url, "text/plain", MAX_ROBOTS_BYTES)
                .await
            {
                Fetched::Reply(reply)
                    if (301..=308).contains(&reply.status) && reply.status != 304 =>
                {
                    let next = reply
                        .location
                        .as_deref()
                        .and_then(|location| robots_url.join(location).ok());
                    match next {
                        Some(next) if Self::origin_key(&next) != key => {
                            break RobotsState::Unavailable("robots_redirect_off_host")
                        }
                        Some(next) if next != robots_url && hops < MAX_ROBOTS_REDIRECTS => {
                            hops += 1;
                            robots_url = next;
                        }
                        _ => break RobotsState::Unavailable("robots_redirect_loop"),
                    }
                }
                Fetched::Reply(reply) => break classify_robots(&reply),
                Fetched::Stopped("deferred") => return RobotsState::Unavailable("domain_paused"),
                Fetched::Stopped(_) | Fetched::BudgetExhausted => {
                    return RobotsState::Unavailable("domain_stopped")
                }
                Fetched::Failed(_) => break RobotsState::Unavailable("robots_unreachable"),
            }
        };
        match &state {
            RobotsState::Blocked(reason) => {
                self.stopped = Some(reason);
                if matches!(*reason, "robots_forbidden" | "robots_challenge") {
                    self.pause_domain(Utc::now() + chrono::Duration::days(BOT_WALL_PAUSE_DAYS))
                        .await;
                }
            }
            RobotsState::RateLimited(until) => {
                self.stopped = Some("rate_limited");
                self.pause_domain(*until).await;
            }
            _ => {}
        }
        self.robots.insert(key, state.clone());
        state
    }

    /// Fetch a page after robots allows the exact path for our agent.
    pub async fn page(&mut self, url: &Url, gray_robots_missing: bool) -> (Fetched, PageState) {
        self.page_limited(url, gray_robots_missing, MAX_PAGE_BYTES)
            .await
    }

    /// [`Self::page`] with a body cap other than [`MAX_PAGE_BYTES`] (a hub
    /// list page may be larger; the cap is the door's, at most 4 MiB). Every
    /// other rule is the same.
    pub async fn page_limited(
        &mut self,
        url: &Url,
        gray_robots_missing: bool,
        max_bytes: usize,
    ) -> (Fetched, PageState) {
        let max_bytes = max_bytes.clamp(64 * 1024, 4 * 1_048_576);
        let robots = self.robots(url).await;
        match robots.allows(&robots_target(url), gray_robots_missing) {
            Some(true) => {}
            Some(false) => return (Fetched::Stopped("robots_disallow"), PageState::Disallowed),
            None => return (Fetched::Stopped(robots.label()), PageState::NotProbed),
        }
        if !robots.visit_allowed(self.now()) {
            return (Fetched::Stopped("visit_window"), PageState::NotProbed);
        }
        let fetched = self
            .request(url, "text/html,application/xhtml+xml;q=0.9", max_bytes)
            .await;
        let state = match &fetched {
            Fetched::Reply(reply) => classify_page(reply, url),
            Fetched::Failed(_) => PageState::Transient,
            _ => PageState::NotProbed,
        };
        match &state {
            PageState::BotWall => {
                self.stopped = Some("page_bot_wall");
                self.pause_domain(Utc::now() + chrono::Duration::days(BOT_WALL_PAUSE_DAYS))
                    .await;
            }
            PageState::RateLimited(until) => {
                self.stopped = Some("rate_limited");
                self.pause_domain(*until).await;
            }
            _ => {}
        }
        (fetched, state)
    }

    /// Fetch a text file (a sitemap, or the opt-out proof a publisher placed
    /// under `/.well-known/`) after robots allows the path. A missing robots
    /// file allows only when `gray_robots_missing` does (G2); 429 pauses
    /// the domain.
    pub async fn text_file(
        &mut self,
        url: &Url,
        max_bytes: usize,
        gray_robots_missing: bool,
    ) -> Fetched {
        let robots = self.robots(url).await;
        match robots.allows(&robots_target(url), gray_robots_missing) {
            Some(true) => {}
            Some(false) => return Fetched::Stopped("robots_disallow"),
            None => return Fetched::Stopped(robots.label()),
        }
        if !robots.visit_allowed(self.now()) {
            return Fetched::Stopped("visit_window");
        }
        let fetched = self.request(url, "text/plain,*/*;q=0.1", max_bytes).await;
        if let Fetched::Reply(reply) = &fetched {
            if reply.status == 429 {
                self.stopped = Some("rate_limited");
                self.pause_domain(reply.retry_at).await;
            }
        }
        fetched
    }

    async fn request(&mut self, url: &Url, accept: &'static str, max: usize) -> Fetched {
        if let Some(reason) = self.stopped {
            return Fetched::Stopped(reason);
        }
        if self.requests >= self.max_requests {
            return Fetched::BudgetExhausted;
        }
        let interval = self.interval(url);
        if let Some(last) = self.last_request {
            tokio::time::sleep_until(last + interval).await;
        }
        if let (Some(pool), Some(domain_id)) = (self.pool, self.domain_id) {
            match reserve_policy_slot(pool, domain_id, interval).await {
                Ok(true) => {}
                Ok(false) => {
                    self.stopped = Some("deferred");
                    return Fetched::Stopped("deferred");
                }
                Err(error) => return Fetched::Failed(error.to_string()),
            }
        }
        self.requests += 1;
        let result = self.http.get(url, accept, max).await;
        self.last_request = Some(Instant::now());
        if let (Some(pool), Some(domain_id)) = (self.pool, self.domain_id) {
            // The slot covered the request window; the next one may start
            // one interval after this request finished.
            let _ = sqlx::query(
                "UPDATE benefit_source_domains
                 SET next_request_at = GREATEST(now() + make_interval(secs => $2),
                                                CASE WHEN next_request_at > now() + make_interval(secs => $3)
                                                     THEN next_request_at END),
                     updated_at = now()
                 WHERE id = $1",
            )
            .bind(domain_id)
            .bind(interval.as_secs_f64())
            .bind(interval.max(Duration::from_secs(30)).as_secs_f64())
            .execute(pool)
            .await;
        }
        match result {
            Ok(reply) => Fetched::Reply(reply),
            Err(FetchError::Transient(message)) | Err(FetchError::Refused(message)) => {
                Fetched::Failed(message)
            }
        }
    }

    async fn pause_domain(&self, until: DateTime<Utc>) {
        if let (Some(pool), Some(domain_id)) = (self.pool, self.domain_id) {
            let _ = sqlx::query(
                "UPDATE benefit_source_domains
                 SET next_request_at = GREATEST(COALESCE(next_request_at, now()), $2), updated_at = now()
                 WHERE id = $1",
            )
            .bind(domain_id)
            .bind(until)
            .execute(pool)
            .await;
        }
    }
}

/// Reserves the shared per-domain request slot without requiring a crawl
/// approval (the census and the evidence job are policy-gated themselves).
/// The slot covers the whole request window, so no other collector using
/// `next_request_at` starts a request to the domain meanwhile. Returns false
/// when the domain is paused for longer than [`MAX_SLOT_WAIT`].
async fn reserve_policy_slot(pool: &PgPool, domain_id: Uuid, interval: Duration) -> Result<bool> {
    let hold = interval.max(Duration::from_secs(30));
    loop {
        let reserved = sqlx::query_scalar::<_, bool>(
            "UPDATE benefit_source_domains
             SET next_request_at = now() + make_interval(secs => $2), updated_at = now()
             WHERE id = $1 AND (next_request_at IS NULL OR next_request_at <= now())
             RETURNING true",
        )
        .bind(domain_id)
        .bind(hold.as_secs_f64())
        .fetch_optional(pool)
        .await?
        .unwrap_or(false);
        if reserved {
            return Ok(true);
        }
        let next: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT next_request_at FROM benefit_source_domains WHERE id = $1")
                .bind(domain_id)
                .fetch_optional(pool)
                .await?
                .flatten();
        let Some(next) = next else {
            return Ok(false);
        };
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_millis(50));
        if wait > MAX_SLOT_WAIT {
            return Ok(false);
        }
        tokio::time::sleep(wait.max(Duration::from_millis(50))).await;
    }
}

#[cfg(test)]
mod tests {
    /// Review A-H2.2: an active partner feed or a refusing stored terms
    /// signal refuses before any request; anything else waits for the page.
    #[test]
    fn known_refusals_need_no_request() {
        assert_eq!(
            refused_without_request(true, None, false),
            Some((Lane::Feed, LaneReason::PartnerFeed))
        );
        assert_eq!(
            refused_without_request(false, Some(TosSignal::SpecificCollection), true),
            Some((Lane::Impossible, LaneReason::TosSpecificCollection))
        );
        assert_eq!(
            refused_without_request(false, Some(TosSignal::GenericAutomated), false),
            Some((Lane::Impossible, LaneReason::TosGenericGrayOff))
        );
        for tos in [
            None,
            Some(TosSignal::None),
            Some(TosSignal::GenericAutomated),
        ] {
            assert_eq!(refused_without_request(false, tos, true), None, "{tos:?}");
        }
    }

    use super::*;

    fn reply(status: u16, content_type: &str, body: &str) -> HttpReply {
        HttpReply {
            status,
            content_type: content_type.into(),
            location: None,
            retry_at: Utc::now(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn robots_variants_are_classified_fail_closed() {
        assert_eq!(
            classify_robots(&reply(404, "text/html", "<html>nf</html>")),
            RobotsState::Missing
        );
        assert_eq!(
            classify_robots(&reply(
                200,
                "text/html",
                "<!doctype html><html><body>app</body></html>"
            )),
            RobotsState::Blocked("robots_html")
        );
        assert_eq!(
            classify_robots(&reply(403, "text/html", "Access Denied")),
            RobotsState::Blocked("robots_forbidden")
        );
        assert_eq!(
            classify_robots(&reply(503, "text/html", "<title>Just a moment...</title>")),
            RobotsState::Blocked("robots_challenge")
        );
        assert_eq!(
            classify_robots(&reply(503, "text/plain", "maintenance")),
            RobotsState::Unavailable("robots_http_error")
        );
        assert!(matches!(
            classify_robots(&reply(429, "text/plain", "")),
            RobotsState::RateLimited(_)
        ));
        let ok = classify_robots(&reply(
            200,
            "text/plain",
            "User-agent: *\nCrawl-delay: 15\nDisallow: /private\n",
        ));
        assert_eq!(ok.crawl_delay(), Some(15.0));
        assert_eq!(ok.allows("/benefit", true), Some(true));
        assert_eq!(ok.allows("/private/x", true), Some(false));
    }

    #[test]
    fn missing_robots_follows_the_g2_switch() {
        assert_eq!(RobotsState::Missing.allows("/x", true), Some(true));
        assert_eq!(RobotsState::Missing.allows("/x", false), Some(false));
    }

    /// LG U+, KT, 현대카드 and 하나카드 (2026-11-06): only named agents, no
    /// `*` group, the root explicitly allowed for some. No group applies to
    /// us, but the file lists who may crawl and we are not on it: every
    /// path is refused.
    #[test]
    fn a_file_naming_only_other_agents_refuses_every_path() {
        let body = "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n\nUser-agent: ClaudeBot\nDisallow: /\n";
        let state = classify_robots(&reply(200, "text/plain", body));
        assert_eq!(state.allows("/benefit", true), Some(false));
        assert_eq!(state.allows("/", true), Some(false));
        assert!(state.search_engine_allowlist());
        // A file that names only other agents refuses us whatever it allows
        // or forbids them (security review H3: the structure decides).
        let restricts = "User-agent: Googlebot-Image\nDisallow: /images\n";
        let state = classify_robots(&reply(200, "text/plain", restricts));
        assert_eq!(state.allows("/benefit", true), Some(false));
        // A file that names us decides by its rules, and is no allowlist.
        let named = "User-agent: MazimiBenefitBot\nAllow: /\n\nUser-agent: Yeti\nAllow: /\n";
        let state = classify_robots(&reply(200, "text/plain", named));
        assert_eq!(state.allows("/benefit", true), Some(true));
        assert!(!state.search_engine_allowlist());
        // A `*` group decides for us (무신사: named crawlers, then
        // `User-agent: *` `Disallow: /`): disallowed by the rules.
        let musinsa = "User-agent: Googlebot\nAllow: /\n\nUser-agent: Yeti\nAllow: /\n\nUser-agent: *\nDisallow: /\n";
        let state = classify_robots(&reply(200, "text/plain", musinsa));
        assert_eq!(state.allows("/benefit", true), Some(false));
        assert!(state.search_engine_allowlist());
        // An empty file names no one: nothing applies, so it allows.
        let empty = classify_robots(&reply(200, "text/plain", "# nothing\n"));
        assert_eq!(empty.allows("/benefit", false), Some(true));
    }

    #[test]
    fn search_engine_allowlist_is_detected_and_our_agent_stays_out() {
        let body = "User-agent: Googlebot\nAllow: /\n\nUser-agent: Yeti\nAllow: /\n\nUser-agent: *\nDisallow: /\n";
        let state = classify_robots(&reply(200, "text/plain", body));
        assert!(state.search_engine_allowlist());
        assert_eq!(
            state.allows("/cjmweb/point-card/brand.do", true),
            Some(false)
        );
        let open = classify_robots(&reply(
            200,
            "text/plain",
            "User-agent: *\nDisallow: /admin\n",
        ));
        assert!(!open.search_engine_allowlist());
    }

    #[test]
    fn query_paths_are_matched_with_their_query() {
        // 동아대: `Disallow: /*?*` beats the shorter `Allow: /` on query URLs.
        let state = classify_robots(&reply(
            200,
            "text/plain",
            "User-agent: *\nAllow: /\nDisallow: /*?*\n",
        ));
        let url = Url::parse("https://www.donga.ac.kr/gzSub/gzSub.aspx?gzSubSiteName=x").unwrap();
        assert_eq!(state.allows(&robots_target(&url), true), Some(false));
        let plain = Url::parse("https://www.donga.ac.kr/notice/1").unwrap();
        assert_eq!(state.allows(&robots_target(&plain), true), Some(true));
    }

    #[test]
    fn our_own_group_wins_over_the_wildcard() {
        let body = "User-agent: MazimiBenefitBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n";
        let state = classify_robots(&reply(200, "text/plain", body));
        assert_eq!(state.allows("/benefit", true), Some(false));
    }

    #[test]
    fn interval_never_drops_below_the_floor() {
        assert_eq!(effective_interval(None, true, 0), Duration::from_secs(10));
        assert_eq!(
            effective_interval(Some(1.0), true, 5_000),
            Duration::from_secs(10)
        );
        assert_eq!(
            effective_interval(Some(25.0), true, 10_000),
            Duration::from_secs(25)
        );
        assert_eq!(
            effective_interval(None, false, 10_000),
            Duration::from_secs(30)
        );
        assert_eq!(
            effective_interval(Some(-5.0), true, 0),
            Duration::from_secs(10)
        );
        assert_eq!(
            effective_interval(None, true, 60_000),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn challenge_pages_are_bot_walls() {
        let url = Url::parse("https://www.canva.com/education/").unwrap();
        assert_eq!(
            classify_page(
                &reply(200, "text/html", "<div id=\"cf-chl-widget\"></div>"),
                &url
            ),
            PageState::BotWall
        );
        assert_eq!(
            classify_page(
                &reply(403, "text/html", "<title>Just a moment...</title>"),
                &url
            ),
            PageState::BotWall
        );
        assert_eq!(
            classify_page(
                &reply(503, "text/html", "<h1>Access Denied</h1> Reference #18"),
                &url
            ),
            PageState::BotWall
        );
        assert_eq!(
            classify_page(&reply(503, "text/html", "busy"), &url),
            PageState::Transient
        );
        assert_eq!(
            classify_page(&reply(404, "text/html", ""), &url),
            PageState::Gone
        );
        assert!(matches!(
            classify_page(&reply(429, "text/html", ""), &url),
            PageState::RateLimited(_)
        ));
    }

    #[test]
    fn redirects_are_classified_by_host() {
        let url = Url::parse("https://frontendmasters.com/welcome/").unwrap();
        let mut moved = reply(301, "text/html", "");
        moved.location = Some("https://master.dev/welcome/".into());
        assert!(matches!(
            classify_page(&moved, &url),
            PageState::Redirect {
                same_host: false,
                ..
            }
        ));
        moved.location = Some("/welcome/students".into());
        assert!(matches!(
            classify_page(&moved, &url),
            PageState::Redirect {
                same_host: true,
                ..
            }
        ));
    }

    #[test]
    fn tos_classifier_separates_login_generic_and_specific_clauses() {
        assert_eq!(
            classify_tos("회원은 서비스를 정상적으로 이용해야 합니다."),
            TosSignal::None
        );
        assert_eq!(
            classify_tos("매크로 등 자동화된 프로그램을 이용한 로그인은 금지됩니다."),
            TosSignal::LoginOnly
        );
        assert_eq!(
            classify_tos(
                "로봇, 스파이더 등 자동화된 수단으로 서비스에 접근하는 행위를 금지합니다."
            ),
            TosSignal::GenericAutomated
        );
        assert_eq!(
            classify_tos("크롤링 등 자동화된 방법으로 사이트의 가격 및 이벤트 정보를 수집하여 제3자에게 제공하는 행위를 금지합니다."),
            TosSignal::SpecificCollection
        );
        assert_eq!(
            classify_tos(
                "You may not use automated means, bots or scrapers to access the service."
            ),
            TosSignal::GenericAutomated
        );
    }

    #[test]
    fn terms_link_must_stay_on_the_host() {
        let page = Url::parse("https://www.example.co.kr/benefit").unwrap();
        let html = r#"<footer><a href="/policy/terms">이용약관</a><a href="/privacy">개인정보처리방침</a></footer>"#;
        assert_eq!(
            find_terms_link(html, &page).unwrap().as_str(),
            "https://www.example.co.kr/policy/terms"
        );
        let foreign = r#"<a href="https://other.example/terms">Terms of Service</a>"#;
        assert!(find_terms_link(foreign, &page).is_none());
    }

    fn lane(input: LaneInput<'_>) -> (Lane, &'static str) {
        choose_lane(&input)
    }

    #[test]
    fn lane_chooser_follows_the_design_order() {
        let ok = RobotsState::Ok {
            body: String::new(),
            crawl_delay: None,
        };
        let html = PageState::Html { text_chars: 4_000 };
        let base = LaneInput {
            opted_out: false,
            feed_active: false,
            robots: &ok,
            path_allowed: Some(true),
            page: &html,
            tos: TosSignal::None,
            gray_generic_tos: false,
            gray_robots_missing: true,
        };
        assert_eq!(lane(base.clone()), (Lane::Direct, "direct_html"));
        assert_eq!(
            lane(LaneInput {
                opted_out: true,
                ..base.clone()
            })
            .0,
            Lane::Impossible
        );
        assert_eq!(
            lane(LaneInput {
                feed_active: true,
                ..base.clone()
            })
            .0,
            Lane::Feed
        );
        assert_eq!(
            lane(LaneInput {
                path_allowed: Some(false),
                ..base.clone()
            }),
            (Lane::Snippet, "robots_disallow")
        );
        let shell = PageState::Html { text_chars: 120 };
        assert_eq!(
            lane(LaneInput {
                page: &shell,
                ..base.clone()
            }),
            (Lane::Render, "js_shell")
        );
        assert_eq!(
            lane(LaneInput {
                page: &PageState::BotWall,
                ..base.clone()
            }),
            (Lane::Secondary, "page_bot_wall")
        );
        let walled = RobotsState::Blocked("robots_forbidden");
        assert_eq!(
            lane(LaneInput {
                robots: &walled,
                ..base.clone()
            })
            .0,
            Lane::Secondary
        );
        assert_eq!(
            lane(LaneInput {
                tos: TosSignal::GenericAutomated,
                ..base.clone()
            }),
            (Lane::Impossible, "tos_generic_gray_off")
        );
        assert_eq!(
            lane(LaneInput {
                tos: TosSignal::GenericAutomated,
                gray_generic_tos: true,
                ..base.clone()
            })
            .0,
            Lane::Direct
        );
        assert_eq!(
            lane(LaneInput {
                tos: TosSignal::SpecificCollection,
                gray_generic_tos: true,
                ..base.clone()
            })
            .0,
            Lane::Impossible
        );
        assert_eq!(
            lane(LaneInput {
                robots: &RobotsState::Missing,
                gray_robots_missing: false,
                ..base.clone()
            })
            .1,
            "robots_missing_gray_off"
        );
        assert_eq!(
            lane(LaneInput {
                page: &PageState::Gone,
                ..base
            })
            .1,
            "page_gone"
        );
    }

    struct NoHttp;

    #[async_trait::async_trait]
    impl PageHttp for NoHttp {
        async fn get(&self, _: &Url, _: &'static str, _: usize) -> Result<HttpReply, FetchError> {
            panic!("a paused domain is never requested");
        }
        fn user_agent(&self) -> &str {
            "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)"
        }
    }

    /// A domain still inside an earlier bot-wall or rate-limit pause is
    /// skipped (A1). The census used to report it as `robots_unavailable`
    /// with robots state `domain_stopped`, which reads like a robots fault
    /// (production 2026-10-24: 7 domains, 79 URLs). It is a pause.
    #[tokio::test]
    async fn a_paused_domain_reads_as_paused_not_as_a_robots_fault() {
        let http = NoHttp;
        let mut session = DomainSession::new(&http, None, None, true, 0, 10);
        session.stopped = Some("deferred");
        let url = Url::parse("https://brand.example/student").unwrap();
        let robots = session.robots(&url).await;
        assert_eq!(robots, RobotsState::Unavailable("domain_paused"));
        let page = PageState::NotProbed;
        assert_eq!(
            choose_lane(&LaneInput {
                opted_out: false,
                feed_active: false,
                robots: &robots,
                path_allowed: None,
                page: &page,
                tos: TosSignal::Unknown,
                gray_generic_tos: true,
                gray_robots_missing: true,
            }),
            (Lane::Impossible, "domain_paused")
        );
    }

    /// F1: every lane reason is classified verdict or not (exhaustive in
    /// `is_verdict`, no default arm), the list is the enum's own, and the
    /// census verdict SQL drops exactly the reasons that are no verdict.
    #[test]
    fn every_lane_reason_is_classified_and_only_passing_replies_are_no_verdict() {
        let texts: std::collections::HashSet<&str> =
            LaneReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            texts.len(),
            LaneReason::ALL.len(),
            "reason texts are unique"
        );
        let no_verdict: Vec<&str> = LaneReason::ALL
            .iter()
            .filter(|r| !r.is_verdict())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(
            no_verdict,
            [
                "rate_limited",
                "domain_paused",
                "robots_unavailable",
                "robots_allowed_unprobed",
                "page_unreachable"
            ]
        );
        // Must stay verdicts: findings about the URL.
        for reason in [
            "robots_disallow",
            "robots_bot_wall",
            "page_bot_wall",
            "js_shell",
            "opt_out",
            "tos_specific_collection",
            "page_gone",
            "direct_html",
        ] {
            assert!(
                LaneReason::ALL
                    .iter()
                    .any(|r| r.as_str() == reason && r.is_verdict()),
                "{reason} is a verdict"
            );
        }
        let sql = census_verdicts_sql("$1");
        for reason in &no_verdict {
            assert!(sql.contains(&format!("'{reason}'")), "{reason} excluded");
        }
        assert!(!sql.contains("'robots_disallow'"));
        // choose_lane writes only reasons of the list.
        let robots = RobotsState::Missing;
        for page in [
            PageState::NotProbed,
            PageState::Html { text_chars: 10 },
            PageState::Html { text_chars: 900 },
            PageState::BotWall,
            PageState::Gone,
            PageState::Transient,
            PageState::Unsupported,
            PageState::Disallowed,
        ] {
            let (_, reason) = choose_lane(&LaneInput {
                opted_out: false,
                feed_active: false,
                robots: &robots,
                path_allowed: Some(true),
                page: &page,
                tos: TosSignal::None,
                gray_generic_tos: false,
                gray_robots_missing: true,
            });
            assert!(texts.contains(reason), "{reason} listed");
        }
    }
}
