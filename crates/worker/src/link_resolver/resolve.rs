//! Design 5.4–5.6 as pure functions: which kind a candidate may be shown
//! as, how check results move its state, when it is checked next, and which
//! candidate a programme shows.
use super::candidates::{Origin, ONE_HOP_FIELD};
use super::classify::{self, Class, Context, Kind, Observation};
use chrono::{DateTime, Duration, Utc};

/// The candidate as the policy sees it.
#[derive(Debug, Clone, Copy)]
pub struct CandidateView<'a> {
    pub url: &'a str,
    pub field: &'a str,
    pub origin: Origin,
    pub free_text: bool,
    /// The programme's title (a one-hop target must name it).
    pub programme: &'a str,
    /// For a one-hop link: the title of the notice page it was found on.
    pub notice_title: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Direct,
    LoginRequired,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::LoginRequired => "login_required",
        }
    }
}

/// What one check means for the link's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The page loaded and is not a dead end.
    Pass,
    /// robots.txt does not let us look: never fetched, never failed.
    Blocked,
    /// No answer, a WAF refusal or a server error (data-centre timeouts on
    /// kosaf, HUG, molit, kepco): counted slowly, never breaks the link.
    Soft(String),
    /// A dead end users would see: gone, closed form, admin page, login
    /// wall, sent to the homepage, unknown host.
    Hard(String),
}

/// Failure sub-reasons that a user's browser would hit as well.
pub fn is_hard_failure(sub: &str) -> bool {
    if let Some(code) = sub.strip_prefix("http_") {
        return !matches!(code, "403" | "408" | "429") && !code.starts_with('5');
    }
    matches!(
        sub,
        "dns" | "form_closed" | "admin_page" | "redirect_to_home" | "login_wall" | "login_page"
    )
}

/// The kind a candidate may be shown as, from its last usable
/// observation. `None`: not shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub kind: Kind,
    pub sub: String,
    pub access: Access,
}

/// Titles of sign-in and identity-check pages.
pub(crate) static GATE_TITLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"로그인|본인\s*인증|권한이\s*없습니다|회원\s*가입").expect("valid")
});
/// Titles of search and listing pages: a form there searches, it does not apply.
pub(crate) static SEARCH_TITLE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"찾기|검색|조회|목록|현황").expect("valid"));
/// Words every programme title shares; they say nothing about which one.
const COMMON_TITLE_WORDS: &[&str] = &[
    "청년",
    "지원",
    "사업",
    "지원사업",
    "신청",
    "모집",
    "운영",
    "프로그램",
    "안내",
    "공고",
    "대상",
    "참여자",
    "및",
    "등",
    "위한",
    "사업안내",
    "지원금",
    "서비스",
];

/// The candidate is where an application happens: an application path,
/// a form service, or a one-hop application link.
fn application_context(view: CandidateView<'_>, kind: Option<Kind>) -> bool {
    view.field == ONE_HOP_FIELD
        || view.origin == Origin::FormHost
        || classify::has_strong_apply_path(view.url)
        || kind == Some(Kind::Application)
}

fn same_site(a: &str, b: &str) -> bool {
    let site = |u: &str| mazimi_core::source_policy::registrable_domain(&classify::host_of(u));
    site(a).is_some() && site(a) == site(b)
}

/// The page asks for a login or an identity check before anything else.
/// Strong: a sign-in redirect, or a gate title together with a password
/// field. Weak: a gate title or a password field alone. A weak signal
/// counts only where an application is expected: a notice with a login
/// box in its sidebar, or a 로그인 word in its title, is still readable.
fn login_gate(view: CandidateView<'_>, observation: &Observation, class: &Class) -> bool {
    let Observation::Fetched {
        final_url, page, ..
    } = observation
    else {
        return false;
    };
    if let Class::Broken { sub } = class {
        return matches!(sub.as_str(), "login_wall" | "form_behind_login")
            && (same_site(view.url, final_url) || view.origin == Origin::FormHost);
    }
    let kind = match class {
        Class::Usable { kind, .. } => Some(*kind),
        Class::Broken { .. } => None,
    };
    page.as_ref().is_some_and(|p| {
        let title = GATE_TITLE.is_match(&p.title);
        if application_context(view, kind) {
            title || p.password
        } else {
            title && p.password
        }
    })
}

/// Liveness of one fresh check (design 5.6 state input).
pub fn liveness(view: CandidateView<'_>, observation: &Observation) -> Liveness {
    let none = |_: &str| 0;
    let ctx = Context {
        shared: false,
        nav_count: &none,
    };
    if matches!(observation, Observation::Blocked) {
        return Liveness::Blocked;
    }
    // The link itself is a sign-in page: never anyone's application.
    if classify::is_login_url(view.url) {
        return Liveness::Hard("login_page".into());
    }
    let class = classify::classify(view.url, Some(observation), &ctx);
    let kind = match &class {
        Class::Usable { kind, .. } => Some(*kind),
        Class::Broken { .. } => None,
    };
    if login_gate(view, observation, &class) {
        // In front of an application: healthy. In front of a notice: users
        // cannot read it.
        return if application_context(view, kind) {
            Liveness::Pass
        } else {
            Liveness::Hard("login_wall".into())
        };
    }
    match class {
        Class::Usable { .. } => Liveness::Pass,
        Class::Broken { sub } if is_hard_failure(&sub) => Liveness::Hard(sub),
        Class::Broken { sub } => Liveness::Soft(sub),
    }
}

/// The page itself is an application form: fields to fill, no password,
/// not a search page.
fn form_signal(page: &classify::PageFacts) -> bool {
    page.forms > 0
        && page.inputs + page.textareas + page.selects >= 4
        && !page.password
        && !SEARCH_TITLE.is_match(&page.title)
}

/// Distinctive words of a programme title (year, common words and
/// one-character words removed).
fn distinctive_words(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.trim_end_matches('년').to_string())
        .filter(|w| w.chars().count() >= 2 && !w.chars().all(|c| c.is_ascii_digit()))
        .filter(|w| !COMMON_TITLE_WORDS.contains(&w.as_str()))
        .collect()
}

/// The target page names the programme: one of its distinctive words
/// appears in the page title (spaces ignored).
pub fn names_programme(programme: &str, page_title: &str) -> bool {
    let title: String = page_title.chars().filter(|c| !c.is_whitespace()).collect();
    distinctive_words(programme)
        .iter()
        .any(|w| title.contains(w.as_str()))
}

/// How the candidate is shown, from `observation`: the last check that
/// passed, or `Blocked` for a robots-disallowed host, or `None` when no
/// check has passed (a soft failure then rests on the URL pattern).
///
/// "신청 페이지로 바로 가기" (application, direct) needs evidence on the page
/// itself: an online form service, an application path with a form, or a
/// 신청 title with a form. A login or identity gate in front of an
/// application is "로그인 후 신청하기". Everything else is a notice at most.
pub fn shown(
    view: CandidateView<'_>,
    observation: Option<&Observation>,
    ctx: &Context<'_>,
) -> Option<Shown> {
    // A link that is itself a sign-in page (review M2: 완주 `&login=login`)
    // leads to no application, whatever happens after the login.
    if view.origin == Origin::Rejected || classify::is_login_url(view.url) {
        return None;
    }
    let one_hop = view.field == ONE_HOP_FIELD;
    let fetched = matches!(observation, Some(Observation::Fetched { .. }));
    if one_hop && !fetched {
        // Design 5.8: a one-hop link is judged only after its own check.
        return None;
    }
    let class = classify::classify(view.url, observation, ctx);
    let gated = observation.is_some_and(|o| login_gate(view, o, &class));
    let page = match observation {
        Some(Observation::Fetched { page, .. }) => page.as_ref(),
        _ => None,
    };
    let (mut kind, mut sub, mut access) = match class {
        Class::Usable { kind, sub, .. } if gated => {
            if !application_context(view, Some(kind)) {
                return None;
            }
            (
                Kind::Application,
                format!("{sub}:login"),
                Access::LoginRequired,
            )
        }
        Class::Usable { kind, sub, .. } => (kind, sub, Access::Direct),
        Class::Broken { sub } if gated && application_context(view, None) => (
            Kind::Application,
            format!("{sub}:application"),
            Access::LoginRequired,
        ),
        Class::Broken { .. } => return None,
    };
    if kind == Kind::Application && access == Access::Direct {
        // Hand-checked on production pages: a 신청 title with a form was
        // almost always an information page whose only forms are the site
        // search and the page-satisfaction survey, so only an online form
        // service or an application path with fields keeps the direct
        // label. A form shared by differently titled programmes is not
        // known to be this one's.
        let direct = match sub.as_str() {
            "online_form" => !ctx.shared,
            "apply_path+form" => page.is_some_and(form_signal),
            _ => false,
        };
        if !direct {
            kind = Kind::Notice;
            sub.push_str(if ctx.shared {
                "+shared_form"
            } else {
                "+no_form"
            });
        }
    }
    if one_hop {
        // Promoted only when the target is an application by itself (a
        // form or a login gate), the notice it was found on names this
        // programme, no other programme lists the target (review M1: 옹진군
        // was sent to the city-wide policy), and a form names the
        // programme: a sidebar "신청하기" often leads to another
        // programme's form.
        let names = page.is_some_and(|p| names_programme(view.programme, &p.title));
        let notice_names = view
            .notice_title
            .is_some_and(|t| names_programme(view.programme, t));
        let promoted = kind == Kind::Application
            && !ctx.shared
            && notice_names
            && (access == Access::LoginRequired || names);
        if !promoted {
            return None;
        }
        sub = format!("one_hop:{sub}");
    }
    let verified = fetched || classify::portal_kind(view.url).is_some();
    let cap = |kind: &mut Kind, sub: &mut String, why: &str| {
        if *kind == Kind::Application {
            *kind = Kind::Notice;
            sub.push_str(why);
        }
    };
    if !verified {
        // robots-disallowed or not reachable from the checker: the URL
        // pattern alone never claims an application.
        cap(&mut kind, &mut sub, "+unverified");
    }
    if view.free_text && view.origin != Origin::Agency {
        cap(&mut kind, &mut sub, "+free_text");
    }
    // A redirect to another site is not this agency's page any more.
    if let Some(Observation::Fetched { final_url, .. }) = observation {
        if view.origin != Origin::FormHost && !same_site(final_url, view.url) {
            cap(&mut kind, &mut sub, "+cross_site");
        }
    }
    // Only a vetted host (public sector, allowlist, form service) gets the
    // application label: an unvetted host controls its own title and paths.
    if !matches!(
        view.origin,
        Origin::PublicSector | Origin::Allowlist | Origin::FormHost
    ) && !super::candidates::is_public_sector(&classify::host_of(view.url))
    {
        cap(&mut kind, &mut sub, "+unvetted_host");
    }
    // Anyone can publish on a blog or video platform: a post there is a
    // notice at most, never the application form.
    if view.origin != Origin::FormHost
        && mazimi_core::source_policy::is_shared_hosting(&classify::host_of(view.url))
    {
        cap(&mut kind, &mut sub, "+shared_host");
    }
    if one_hop && kind != Kind::Application {
        return None;
    }
    if kind != Kind::Application {
        access = Access::Direct;
    }
    Some(Shown { kind, sub, access })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Unchecked,
    /// Never fetched: a portal page or a rejected origin.
    Skipped,
    Ok,
    Blocked,
    SoftFail,
    HardFail,
    Broken,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Skipped => "skipped",
            Self::Ok => "ok",
            Self::Blocked => "blocked",
            Self::SoftFail => "soft_fail",
            Self::HardFail => "hard_fail",
            Self::Broken => "broken",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "skipped" => Self::Skipped,
            "ok" => Self::Ok,
            "blocked" => Self::Blocked,
            "soft_fail" => Self::SoftFail,
            "hard_fail" => Self::HardFail,
            "broken" => Self::Broken,
            _ => Self::Unchecked,
        }
    }
}

/// Hard failures this far apart (or more) confirm a dead link.
pub const HARD_FAILURE_SPACING: Duration = Duration::hours(24);
pub const PASSES_TO_RESTORE: i32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckState {
    pub status: Status,
    pub hard_failures: i32,
    pub first_hard_failure_at: Option<DateTime<Utc>>,
    pub soft_failures: i32,
    pub first_soft_failure_at: Option<DateTime<Utc>>,
    pub restore_passes: i32,
    /// Kept for the stored column; never set: soft failures (timeouts, TLS,
    /// refused connections) never demote a link, only hard failures count.
    pub demoted: bool,
}

/// Design 5.6: two hard failures at least 24 h apart break a link; a
/// broken link that passes twice is restored. Soft failures are counted
/// and recorded but never demote or break a link.
pub fn observe(state: &CheckState, result: &Liveness, now: DateTime<Utc>) -> CheckState {
    let mut next = state.clone();
    let broken = state.status == Status::Broken;
    match result {
        Liveness::Pass => {
            if broken {
                next.restore_passes += 1;
                if next.restore_passes < PASSES_TO_RESTORE {
                    return next;
                }
            }
            next = CheckState {
                status: Status::Ok,
                ..CheckState::default()
            };
        }
        Liveness::Blocked => {
            if !broken {
                next.status = Status::Blocked;
            }
        }
        Liveness::Soft(_) => {
            next.restore_passes = 0;
            if broken {
                return next;
            }
            next.status = Status::SoftFail;
            next.soft_failures += 1;
            next.first_soft_failure_at.get_or_insert(now);
        }
        Liveness::Hard(_) => {
            next.restore_passes = 0;
            if broken {
                return next;
            }
            match next.first_hard_failure_at {
                None => {
                    next.hard_failures = 1;
                    next.first_hard_failure_at = Some(now);
                    next.status = Status::HardFail;
                }
                Some(first) if now - first >= HARD_FAILURE_SPACING => {
                    next.hard_failures += 1;
                    next.status = Status::Broken;
                }
                // A second failure within a day does not confirm anything.
                Some(_) => next.status = Status::HardFail,
            }
        }
    }
    next
}

/// When the candidate is checked next (design 5.6 cadence).
pub fn next_check(
    state: &CheckState,
    selected: bool,
    opens_soon: bool,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let selected_every = if opens_soon {
        Duration::days(1)
    } else {
        Duration::days(7)
    };
    Some(
        now + match state.status {
            Status::Skipped => return None,
            Status::Unchecked => Duration::zero(),
            Status::HardFail => HARD_FAILURE_SPACING + Duration::minutes(5),
            Status::Broken if state.restore_passes > 0 => Duration::days(1),
            Status::Broken => Duration::days(7),
            Status::SoftFail if selected && opens_soon => Duration::days(1),
            Status::SoftFail => Duration::days(5),
            Status::Ok | Status::Blocked if selected => selected_every,
            Status::Ok | Status::Blocked => Duration::days(30),
        },
    )
}

/// One candidate as selection sees it.
#[derive(Debug, Clone)]
pub struct Choice {
    pub shown: Option<Shown>,
    pub field_rank: i16,
    pub status: Status,
    pub demoted: bool,
    /// Currently the programme's selected or notice link.
    pub current: bool,
    /// A hard failure not yet confirmed or cleared by a pass (a soft
    /// failure since then does not clear it).
    pub unconfirmed_hard: bool,
}

/// Design 5.3: the lowest (kind rank, field rank); broken and unchecked
/// candidates never win; a first unconfirmed hard failure keeps a current
/// link until it is confirmed; a demoted link loses to every other kind.
pub fn select(options: &[Choice]) -> (Option<usize>, Option<usize>) {
    let eligible = |o: &Choice| {
        o.shown.is_some()
            && match o.status {
                Status::Broken | Status::Unchecked => false,
                Status::HardFail => o.current,
                _ if o.unconfirmed_hard => o.current,
                _ => true,
            }
    };
    let key = |o: &Choice| {
        let kind = o.shown.as_ref().map(|s| s.kind.rank()).unwrap_or(9);
        (kind + if o.demoted { 4 } else { 0 }, o.field_rank)
    };
    let best_of = |notice_only: bool| {
        options
            .iter()
            .enumerate()
            .filter(|(_, o)| eligible(o))
            .filter(|(_, o)| {
                !notice_only
                    || o.shown
                        .as_ref()
                        .is_some_and(|s| s.kind != Kind::Application)
            })
            .min_by_key(|(_, o)| key(o))
            .map(|(i, _)| i)
    };
    (best_of(false), best_of(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_resolver::classify::PageFacts;

    fn t(hours: i64) -> DateTime<Utc> {
        "2026-10-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::hours(hours)
    }
    fn hard() -> Liveness {
        Liveness::Hard("http_404".into())
    }
    fn soft() -> Liveness {
        Liveness::Soft("unreachable:timeout".into())
    }

    #[test]
    fn two_hard_failures_a_day_apart_break_a_link_and_two_passes_restore_it() {
        let s = observe(&CheckState::default(), &hard(), t(0));
        assert_eq!(s.status, Status::HardFail);
        // Within 24 h: not confirmed.
        let s = observe(&s, &hard(), t(23));
        assert_eq!(s.status, Status::HardFail);
        let s = observe(&s, &hard(), t(24));
        assert_eq!(s.status, Status::Broken);
        let s = observe(&s, &Liveness::Pass, t(30));
        assert_eq!(s.status, Status::Broken);
        let s = observe(&s, &Liveness::Pass, t(60));
        assert_eq!(
            s,
            CheckState {
                status: Status::Ok,
                ..CheckState::default()
            }
        );
        // A pass between failures resets the clock.
        let s = observe(&CheckState::default(), &hard(), t(0));
        let s = observe(&s, &Liveness::Pass, t(10));
        let s = observe(&s, &hard(), t(30));
        assert_eq!(s.status, Status::HardFail);
    }

    /// Product decision: data-centre timeouts (kosaf, HUG, molit, kepco)
    /// must never take a link down, however long they last.
    #[test]
    fn soft_failures_never_demote_or_break_a_link() {
        let mut s = CheckState::default();
        for day in 0..60 {
            s = observe(&s, &soft(), t(day * 24));
            assert_eq!((s.status, s.demoted), (Status::SoftFail, false));
        }
        assert_eq!(s.soft_failures, 60);
        assert_eq!(observe(&s, &Liveness::Pass, t(61 * 24)).status, Status::Ok);
    }

    #[test]
    fn cadence_follows_the_design() {
        let ok = CheckState {
            status: Status::Ok,
            ..CheckState::default()
        };
        assert_eq!(next_check(&ok, true, false, t(0)), Some(t(7 * 24)));
        assert_eq!(next_check(&ok, true, true, t(0)), Some(t(24)));
        assert_eq!(next_check(&ok, false, true, t(0)), Some(t(30 * 24)));
        assert_eq!(
            next_check(&CheckState::default(), false, false, t(0)),
            Some(t(0))
        );
        let skipped = CheckState {
            status: Status::Skipped,
            ..CheckState::default()
        };
        assert_eq!(next_check(&skipped, true, true, t(0)), None);
    }

    fn view<'a>(
        url: &'a str,
        field: &'a str,
        origin: Origin,
        free_text: bool,
    ) -> CandidateView<'a> {
        CandidateView {
            url,
            field,
            origin,
            free_text,
            programme: "2026년 청년 공간 이용 지원",
            notice_title: None,
        }
    }
    fn none(_: &str) -> usize {
        0
    }

    fn facts(final_url: &str, title: &str, forms: u32, fields: u32, password: bool) -> Observation {
        Observation::Fetched {
            status: 200,
            final_url: final_url.into(),
            page: Some(PageFacts {
                title: title.into(),
                forms,
                inputs: fields,
                password,
                ..PageFacts::default()
            }),
        }
    }
    fn label(v: CandidateView<'_>, obs: &Observation) -> Option<(Kind, Access)> {
        let ctx = Context {
            shared: false,
            nav_count: &none,
        };
        shown(v, Some(obs), &ctx).map(|s| (s.kind, s.access))
    }
    fn hop<'a>(url: &'a str, programme: &'a str) -> CandidateView<'a> {
        CandidateView {
            url,
            field: ONE_HOP_FIELD,
            origin: Origin::PublicSector,
            free_text: false,
            programme,
            notice_title: Some(programme),
        }
    }
    const DIRECT: Option<(Kind, Access)> = Some((Kind::Application, Access::Direct));
    const LOGIN: Option<(Kind, Access)> = Some((Kind::Application, Access::LoginRequired));
    const NOTICE: Option<(Kind, Access)> = Some((Kind::Notice, Access::Direct));

    #[test]
    fn free_text_robots_and_unfetched_links_never_claim_an_application() {
        let ctx = Context {
            shared: false,
            nav_count: &none,
        };
        let url = "https://apply.example.go.kr/youth/aplyReqst.do?id=1";
        let form = facts(url, "청년 월세", 1, 6, false);
        let agency = view(url, "aplyUrlAddr", Origin::PublicSector, false);
        assert_eq!(label(agency, &form), DIRECT);
        assert_eq!(
            shown(agency, Some(&Observation::Blocked), &ctx)
                .unwrap()
                .kind,
            Kind::Notice
        );
        assert_eq!(shown(agency, None, &ctx).unwrap().kind, Kind::Notice);
        assert_eq!(
            label(
                view(url, "plcyAplyMthdCn_text", Origin::PublicSector, true),
                &form
            ),
            NOTICE
        );
        assert_eq!(
            label(
                view(url, "plcyAplyMthdCn_text", Origin::Agency, true),
                &form
            ),
            DIRECT
        );
        assert_eq!(
            label(view(url, "refUrlAddr1", Origin::Rejected, false), &form),
            None
        );
        let blog = "https://blog.naver.com/hpcounty/224187917962";
        let post = shown(
            view(blog, "refUrlAddr1", Origin::Agency, false),
            Some(&facts(blog, "청년 결혼축하금 신청", 1, 6, false)),
            &ctx,
        )
        .unwrap();
        assert_eq!(
            (post.kind, post.sub.as_str()),
            (Kind::Notice, "title_apply_page+no_form")
        );
    }

    #[test]
    fn unvetted_hosts_and_cross_site_redirects_are_notices_at_most() {
        let url = "https://www.kupc.or.kr/apply/aplyReqst.do?id=1";
        let form = facts(url, "신청", 1, 6, false);
        assert_eq!(
            label(view(url, "refUrlAddr1", Origin::Agency, false), &form),
            NOTICE
        );
        assert_eq!(
            label(view(url, "refUrlAddr1", Origin::Allowlist, false), &form),
            DIRECT
        );
        let gov = "https://www.example.go.kr/youth/aplyReqst.do?id=1";
        let away = facts(
            "https://squatter.example.com/youth/aplyReqst.do?id=1",
            "신청",
            1,
            6,
            false,
        );
        assert_eq!(
            label(view(gov, "aplyUrlAddr", Origin::PublicSector, false), &away),
            NOTICE
        );
    }

    /// The direct label needs an online form service or an application
    /// path with fields (review HIGH 2, then the hand check).
    #[test]
    fn a_title_or_path_without_a_form_is_a_notice() {
        let gov = |u: &'static str| view(u, "온라인신청사이트URL", Origin::PublicSector, false);
        // Review case 8: 광명 입학 축하금, an info page with an apply button.
        let u = "https://www.gm.go.kr/pt/enterSchool/enterSchoolView.do?programSn=2024";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "초중고등학생 입학 축하금 신청", 0, 0, false)
            ),
            NOTICE
        );
        // Case 9: umppa 지원사업신청 detail page (5 forms, 1 field).
        let u = "https://umppa.seoul.go.kr/hmpg/sprt/bzin/bzmgComtDetail.do?biz_mng_no=1";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "지원사업신청 < 탄생육아 몽땅정보통", 5, 1, false)
            ),
            NOTICE
        );
        // Case 10: nongupez 사업상세 under an /bizAply/ path.
        let u = "https://www.nongupez.go.kr/nsm/bizAply/wholeBiz/wholeBizDtls?afbzCd=AB000609";
        assert_eq!(label(gov(u), &facts(u, "사업상세", 0, 0, false)), NOTICE);
        // Case 7: work24 "훈련 찾기·신청", a search form with 53 inputs.
        let u = "https://www.work24.go.kr/hr/a/a/1100/trnnCrsInf8.do?x=1";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "K-뉴딜 아카데미 훈련과정 | 훈련 찾기·신청", 4, 56, false)
            ),
            NOTICE
        );
        // Hand check: 원주 "프로그램 신청" views and 부산 신청 pages, whose
        // only forms are the site search and the satisfaction survey.
        let u = "https://www.wonju.go.kr/wjyouth/viewTnWjyouthProgrmU.do?progrmNo=71&key=5942";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "프로그램 신청 - 원주시 청년지원센터", 3, 9, false)
            ),
            NOTICE
        );
        let u = "https://www.busan.go.kr/depart/consolation02";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "신청하기 : 부산광역시 분야별 통합", 4, 8, false)
            ),
            NOTICE
        );
        // Kept: an application path with fields, an online form service.
        let u =
            "https://www.gwanak.go.kr/site/gwanak/reservation/reservationView.do?riIdx=RI001013";
        assert_eq!(
            label(
                gov(u),
                &facts(u, "무료법률상담예약 : 관악구청", 3, 12, false)
            ),
            DIRECT
        );
        let f = "https://docs.google.com/forms/d/e/1FAIpQL/viewform";
        assert_eq!(
            label(
                view(f, "aplyUrlAddr", Origin::FormHost, false),
                &facts(f, "신청서", 0, 0, false)
            ),
            DIRECT
        );
        // Hand check: one naver.me form listed by three differently titled
        // programmes (방송 캠프, 재테크 강좌, 요가 강습).
        let shared = Context {
            shared: true,
            nav_count: &none,
        };
        let s = shown(
            view(f, "aplyUrlAddr", Origin::FormHost, false),
            Some(&facts(f, "네이버 폼", 0, 0, false)),
            &shared,
        )
        .unwrap();
        assert_eq!(
            (s.kind, s.sub.as_str()),
            (Kind::Notice, "online_form+shared_form")
        );
    }

    /// Review HIGH 3: login and identity gates never carry the direct label.
    #[test]
    fn login_and_identity_gates_are_login_required_or_a_login_wall() {
        // Case 3 / re-review M2: 완주 `...&login=login` is the site's own
        // 마이페이지 login page, not an application behind a login: not shown.
        let u = "https://youth.wanju.go.kr/index.wanju?menuCd=DOM_000000107010000000&login=login";
        let gate = facts(u, "로그인<마이페이지:청년메카완주", 2, 4, true);
        assert_eq!(label(hop(u, "완주군 청년참여예산제 운영"), &gate), None);
        assert_eq!(
            liveness(hop(u, "완주군 청년참여예산제 운영"), &gate),
            Liveness::Hard("login_page".into())
        );
        // Case 4: 수영구 board write → 본인인증 page.
        let u = "https://www.suyeong.go.kr/board/write.suyeong?boardId=BBS_0000341";
        let id = facts(
            "https://www.suyeong.go.kr/index.suyeong?menuCd=DOM_1&messageStr=x",
            "본인인증 |부산광역시 수영구청",
            3,
            7,
            false,
        );
        assert_eq!(label(hop(u, "청년 성장 프로젝트"), &id), LOGIN);
        // Incheon AcptRegForm: a "log in first" page without a redirect.
        let u = "https://youth.incheon.go.kr/youthpolicy/youthPolicyAcptRegForm.do?poly_seq=403";
        assert_eq!(
            label(
                hop(u, "2026년 청년 웰컴페이"),
                &facts(u, "로그인을 하신 후에 이용 가능합니다.", 0, 0, false)
            ),
            LOGIN
        );
        // An application path redirected to a sign-in page.
        let u = "https://www.example.go.kr/youth/aplyReqst.do?id=1";
        let login = facts(
            "https://www.example.go.kr/member/login.do?returnUrl=x",
            "로그인",
            1,
            2,
            true,
        );
        let v = view(u, "aplyUrlAddr", Origin::PublicSector, false);
        assert_eq!(liveness(v, &login), Liveness::Pass);
        assert_eq!(label(v, &login), LOGIN);
        // A notice behind a gate is a dead end; a sidebar login box is not a gate.
        let n = "https://www.example.go.kr/youth/notice/view.do?idx=3";
        let notice = view(n, "refUrlAddr1", Origin::PublicSector, false);
        assert_eq!(
            liveness(notice, &login),
            Liveness::Hard("login_wall".into())
        );
        // Re-review L2: a gate word in a notice title alone, or a sidebar
        // login box alone, is not a login wall; both together are.
        assert_eq!(
            liveness(notice, &facts(n, "권한이 없습니다", 0, 0, false)),
            Liveness::Pass
        );
        assert_eq!(label(notice, &facts(n, "청년 공고", 1, 2, true)), NOTICE);
        assert_eq!(
            liveness(notice, &facts(n, "로그인 | 구청", 1, 2, true)),
            Liveness::Hard("login_wall".into())
        );
        // Re-review L1: login pages by segment or key, not substring.
        for u in [
            "https://www.example.go.kr/member/login.do?returnUrl=x",
            "https://cpms.childcare.go.kr/loginF2.jsp",
            "https://www.suyeong.go.kr/index.suyeong?loginMode=login&menuCd=1",
            "https://lopy.komscochak.com/flow/login/",
            "https://nid.naver.com/nidlogin.login",
        ] {
            assert!(classify::is_login_url(u), "{u}");
        }
        for u in [
            "https://kosis.kr/statHtml/statHtml.do?sso=ok&tblId=DT_170002_A001",
            "https://www.example.go.kr/author/list.do",
            "https://www.example.go.kr/member/join.do",
        ] {
            assert!(!classify::is_login_url(u), "{u}");
        }
    }

    /// Review HIGH 1: a sidebar or hub "신청하기" link is not this
    /// programme's application.
    #[test]
    fn one_hop_links_are_promoted_only_as_this_programmes_application() {
        // Case 1: 영등포 sidebar form of 청년 행정체험단.
        let u = "https://www.ydp.go.kr/www/selectTnPrtimeJoListU.do?key=6018&";
        let o = facts(
            u,
            "신청하기 < 청년 행정체험단 < 구민참여 < 영등포구청",
            2,
            6,
            false,
        );
        assert_eq!(label(hop(u, "청년 국가자격시험 응시료 지원"), &o), None);
        // Even when it names the programme, a 신청 title is not a form.
        assert_eq!(label(hop(u, "청년 행정체험단 모집"), &o), None);
        let apply = "https://www.ydp.go.kr/www/aplyReqst.do?key=6018";
        let o = facts(apply, "청년 행정체험단 신청", 1, 8, false);
        assert_eq!(label(hop(apply, "청년 행정체험단 모집"), &o), DIRECT);
        assert_eq!(label(hop(apply, "청년 국가자격시험 응시료 지원"), &o), None);
        // Case 2: 영천 redirect to the 전입지원금 form.
        let u = "https://www.yc.go.kr/depart/contents.do?mId=1002000000";
        let o = facts(
            "https://www.yc.go.kr/depart/transferSupport/write.do?mId=1002000000",
            "전입지원금 신청하기 | 인구늘리기 | 영천시청",
            3,
            8,
            false,
        );
        assert_eq!(
            label(hop(u, "청년창업 특화거리 점포 리모델링 및 간판"), &o),
            None
        );
        // Case 5: 용인 programme info page without a form.
        let u = "https://www.yongin.go.kr/home/www/www18/www18_06/www18_06_02/www18_06_02_06.jsp";
        let o = facts(
            u,
            "청년부동산중개보수감면 < 청년지원 < 용인시청",
            0,
            1,
            false,
        );
        assert_eq!(
            label(hop(u, "용인청년 부동산 중개 보수 감면 사업"), &o),
            None
        );
        // Case 6: 창원 platform-wide 신청·접수하기 hub (3 fields).
        let u = "https://www.changwon.go.kr/youth/05085/05111/05112.web";
        let o = facts(u, "신청·접수하기 | 창원청년정보플랫폼", 3, 3, false);
        assert_eq!(label(hop(u, "청년 면접수당 지원사업"), &o), None);
        // Hand check: 수영 U-도서관 search page for 임산부영유아 도서 택배.
        let u = "https://www.suyeong.go.kr/library/index.suyeong?menuCd=DOM_000001604002000000";
        let o = facts(
            u,
            "U-도서관 신청하기 |수영U도서관 | 수영구 도서관",
            3,
            20,
            false,
        );
        assert_eq!(label(hop(u, "임산부영유아 도서 택배 서비스"), &o), None);
        // Re-review M1: 옹진군 sent to the city-wide policy that another
        // programme also lists, and a target whose notice names another
        // programme, are never promoted.
        let u =
            "https://youth.incheon.go.kr/youthpolicy/youthPolicyAcptSelfCheckForm.do?poly_seq=412";
        let gate = facts(u, "로그인을 하신 후에 이용 가능합니다.", 0, 0, false);
        let both = Context {
            shared: true,
            nav_count: &none,
        };
        assert!(shown(
            hop(u, "인천광역시 옹진군 청년 자격증 응시료 지원"),
            Some(&gate),
            &both
        )
        .is_none());
        let mut v = hop(u, "(옹진군) 청년 자격증 응시료 지원");
        assert_eq!(label(v, &gate), LOGIN);
        v.notice_title = Some("인천청년정책-(미추홀구) 청년 면접수당 지원사업 | 인천청년포털");
        assert_eq!(label(v, &gate), None);
        v.notice_title = None;
        assert_eq!(label(v, &gate), None);
        // Never judged before its own check.
        let ctx = Context {
            shared: false,
            nav_count: &none,
        };
        assert!(shown(hop(u, "청년 면접수당"), None, &ctx).is_none());
        assert!(names_programme(
            "청년 행정체험단 모집",
            "신청하기 < 청년 행정체험단"
        ));
        assert!(!names_programme("2026년 청년 지원 사업", "청년 지원 신청"));
    }

    fn opt(kind: Kind, rank: i16, status: Status) -> Choice {
        Choice {
            shown: Some(Shown {
                kind,
                sub: String::new(),
                access: Access::Direct,
            }),
            field_rank: rank,
            status,
            demoted: false,
            unconfirmed_hard: false,
            current: false,
        }
    }

    #[test]
    fn selection_is_lexicographic_and_never_picks_broken() {
        let options = vec![
            opt(Kind::Portal, 0, Status::Ok),
            opt(Kind::Notice, 2, Status::Ok),
            opt(Kind::Notice, 1, Status::SoftFail),
            opt(Kind::Information, 3, Status::Skipped),
            opt(Kind::Application, 4, Status::Broken),
            opt(Kind::Application, 5, Status::Unchecked),
        ];
        assert_eq!(select(&options), (Some(2), Some(2)));
        let mut options = options;
        options[2].demoted = true;
        assert_eq!(select(&options).0, Some(1));
        let mut options = vec![
            opt(Kind::Application, 3, Status::HardFail),
            opt(Kind::Notice, 0, Status::Ok),
        ];
        assert_eq!(select(&options), (Some(1), Some(1)));
        options[0].current = true;
        assert_eq!(select(&options), (Some(0), Some(1)));
        assert_eq!(
            select(&[opt(Kind::Notice, 0, Status::Broken)]),
            (None, None)
        );
    }
}
