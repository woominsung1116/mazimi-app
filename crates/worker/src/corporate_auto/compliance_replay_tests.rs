//! Replay tests for robots handling on corporate hosts and for robots
//! `Visit-time`. Each one
//! drives the production [`DomainSession`] against a scripted transport
//! that records every request, so "never requested" is a fact of the run.

use super::http::{FetchError, HttpReply, PageHttp};
use super::policy::{self, DomainSession, Fetched, PageState};
use chrono::{TimeZone, Utc};
use reqwest::Url;
use std::sync::Mutex;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/doors_20261110/robots"
);

fn robots_fixture(host: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{host}.txt")).expect("robots fixture")
}

/// Answers from a fixed script and records every URL requested.
pub(crate) struct ScriptedHttp {
    replies: Vec<(String, HttpReply)>,
    pub(crate) requested: Mutex<Vec<String>>,
}

pub(crate) fn reply(status: u16, content_type: &str, body: &str) -> HttpReply {
    HttpReply {
        status,
        content_type: content_type.into(),
        location: None,
        retry_at: Utc::now(),
        body: body.as_bytes().to_vec(),
    }
}

impl ScriptedHttp {
    pub(crate) fn new(replies: Vec<(&str, HttpReply)>) -> Self {
        Self {
            replies: replies
                .into_iter()
                .map(|(url, reply)| (url.to_string(), reply))
                .collect(),
            requested: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn requested(&self) -> Vec<String> {
        self.requested.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl PageHttp for ScriptedHttp {
    async fn get(&self, url: &Url, _: &'static str, _: usize) -> Result<HttpReply, FetchError> {
        self.requested.lock().unwrap().push(url.to_string());
        self.replies
            .iter()
            .find(|(u, _)| u == url.as_str())
            .map(|(_, r)| r.clone())
            .ok_or_else(|| FetchError::Transient("not scripted".into()))
    }
    fn user_agent(&self) -> &str {
        "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)"
    }
}

fn session(http: &ScriptedHttp) -> DomainSession<'_> {
    DomainSession::new(http, None, None, true, 0, 20)
        .with_test_spacing(std::time::Duration::from_millis(1))
}

/// Robots is read before any other request, and a `/search*` path that
/// robots disallows is refused without requesting it.
#[tokio::test]
async fn itunes_search_path_is_refused_without_a_request() {
    let http = ScriptedHttp::new(vec![(
        "https://itunes.apple.com/robots.txt",
        reply(200, "text/plain", &robots_fixture("itunes.apple.com")),
    )]);
    let mut session = session(&http);
    let url =
        Url::parse("https://itunes.apple.com/search?term=cgv&country=kr&entity=software").unwrap();
    let (fetched, state) = session.page(&url, false).await;
    assert_eq!(state, PageState::Disallowed);
    assert_eq!(fetched, Fetched::Stopped("robots_disallow"));
    assert_eq!(
        http.requested(),
        vec!["https://itunes.apple.com/robots.txt"]
    );
}

/// Robots is the first request to any host, and nothing else is requested
/// once it disallows the path (here `Disallow: /`).
#[tokio::test]
async fn a_disallow_all_host_gets_no_request_but_robots() {
    let http = ScriptedHttp::new(vec![(
        "https://t-id.co.kr/robots.txt",
        reply(200, "text/plain", &robots_fixture("t-id.co.kr")),
    )]);
    let mut session = session(&http);
    for path in [
        "/tc/TWDT/TERMS_OF_SERVICE/content?title=true",
        "/",
        "/terms",
    ] {
        let url = Url::parse(&format!("https://t-id.co.kr{path}")).unwrap();
        let (_, state) = session.page(&url, false).await;
        assert_eq!(state, PageState::Disallowed, "{path}");
        assert_eq!(
            session.text_file(&url, 1024, false).await,
            Fetched::Stopped("robots_disallow")
        );
    }
    assert_eq!(http.requested(), vec!["https://t-id.co.kr/robots.txt"]);
}

/// The resolver re-checks robots on every redirect hop and never requests a
/// disallowed hop, and an off-host redirect is never followed at all.
#[tokio::test]
async fn redirects_into_a_disallowed_path_or_off_the_host_are_not_followed() {
    let mut into_member = reply(302, "text/html", "");
    into_member.location = Some("/member/membership".into());
    let mut off_host = reply(302, "text/html", "");
    off_host.location = Some("https://other-host.example/membership".into());
    let http = ScriptedHttp::new(vec![
        (
            "https://nol.yanolja.com/robots.txt",
            reply(200, "text/plain", &robots_fixture("nol.yanolja.com")),
        ),
        ("https://nol.yanolja.com/membership", into_member),
        ("https://nol.yanolja.com/benefit", off_host),
    ]);
    let mut session = session(&http);
    let start = Url::parse("https://nol.yanolja.com/membership").unwrap();
    let resolved = policy::fetch_resolving_same_host(&mut session, &start, false, None).await;
    assert_eq!(resolved.state, PageState::Disallowed);
    assert_eq!(resolved.url.path(), "/member/membership");
    let start = Url::parse("https://nol.yanolja.com/benefit").unwrap();
    let resolved = policy::fetch_resolving_same_host(&mut session, &start, false, None).await;
    assert!(matches!(
        resolved.state,
        PageState::Redirect {
            same_host: false,
            ..
        }
    ));
    let requested = http.requested();
    assert!(
        !requested.iter().any(|u| u.contains("/member/")),
        "{requested:?}"
    );
    assert!(
        !requested.iter().any(|u| u.contains("other-host")),
        "{requested:?}"
    );
}

/// robots `Visit-time` (www.gsretail.com: `0400-0845` UTC). Outside the
/// window robots is read and nothing else is requested; inside it the page
/// is requested.
#[tokio::test]
async fn visit_time_outside_the_window_sends_no_page_request() {
    let robots = robots_fixture("www.gsretail.com");
    let page = "https://www.gsretail.com/brand/gs25";
    let http = ScriptedHttp::new(vec![
        (
            "https://www.gsretail.com/robots.txt",
            reply(200, "text/plain", &robots),
        ),
        (
            page,
            reply(200, "text/html", "<html><body>gs25</body></html>"),
        ),
    ]);
    let url = Url::parse(page).unwrap();
    let mut outside =
        session(&http).with_test_clock(Utc.with_ymd_and_hms(2026, 11, 10, 14, 14, 0).unwrap());
    let (fetched, state) = outside.page(&url, false).await;
    assert_eq!(fetched, Fetched::Stopped("visit_window"));
    assert_eq!(state, PageState::NotProbed);
    assert_eq!(
        outside.text_file(&url, 1024, false).await,
        Fetched::Stopped("visit_window")
    );
    assert_eq!(
        http.requested(),
        vec!["https://www.gsretail.com/robots.txt"]
    );
    assert_eq!(outside.stopped(), None, "a closed window is not a stop");

    let mut inside =
        session(&http).with_test_clock(Utc.with_ymd_and_hms(2026, 11, 10, 5, 0, 0).unwrap());
    let (_, state) = inside.page(&url, false).await;
    assert!(matches!(state, PageState::Html { .. }), "{state:?}");
    assert_eq!(http.requested().last().map(String::as_str), Some(page));
}
