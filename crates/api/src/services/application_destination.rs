//! A navigation destination is evidence of a link, never submission authority.
use mazimi_core::source_policy::canonicalize_public_https_url;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Destination {
    pub url: String,
    pub origin: String,
    pub platform: String,
    pub kind: &'static str,
    pub evidence: &'static str,
    pub label: &'static str,
    pub explanation: &'static str,
}

/// Classify the program's official URL. Only HTTPS links to public hosts
/// without embedded credentials become destinations. `application_steps`
/// are no longer consulted: operational data never carried a usable step URL
/// and a step title is not evidence of a working form.
/// The name a reader knows a destination's site by: the portal's own name,
/// else its host without "www.".
pub fn site_name(destination: &Destination) -> String {
    match destination.platform.as_str() {
        "government24" => "정부24",
        "bokjiro" => "복지로",
        "youth_center" => "온통청년",
        "work24" => "고용24",
        "kosaf" => "한국장학재단",
        host => host.trim_start_matches("www."),
    }
    .to_owned()
}

pub fn resolve(official: Option<&str>) -> Option<Destination> {
    destination(official?, "official_url")
}

fn destination(url: &str, evidence: &'static str) -> Option<Destination> {
    let value = url.trim();
    let parsed = canonicalize_public_https_url(value).ok()?;
    let host = parsed.domain.trim_start_matches("www.");
    let path = parsed.url.path();
    let platform = match host {
        "gov.kr" | "plus.gov.kr" => "government24",
        "bokjiro.go.kr" => "bokjiro",
        "youthcenter.go.kr" => "youth_center",
        "work24.go.kr" => "work24",
        "kosaf.go.kr" => "kosaf",
        _ => host,
    }
    .to_string();
    let kind = if (host == "youthcenter.go.kr" && path.contains("ythPlcyDetail"))
        || (host == "gov.kr" && path.contains("rcvfvrSvc"))
        || (host == "bokjiro.go.kr" && path.contains("wlfareInfo"))
    {
        "information"
    } else if mazimi_core::application_resources::is_generic_portal(value)
        || (host == "finlife.fss.or.kr" && path == "/main/main.do")
    {
        "portal"
    } else {
        // A path such as /apply is not proof that a working form exists.
        "unverified_page"
    };
    Some(Destination {
        // Do not strip fragments or query keys from a navigation URL: they may
        // identify the form in an SPA. The canonicalizer is validation only.
        url: value.to_owned(),
        origin: parsed.url.origin().ascii_serialization(),
        platform,
        kind,
        evidence,
        label: if kind == "portal" { "공식 사이트에서 확인" } else { "공식 안내에서 계속" },
        explanation: match kind {
            "information" => "공고 안내 페이지로 연결돼요. 안내된 접수 방식에 따라 신청을 이어가세요.",
            "portal" => "공식 사이트로 연결돼요. 해당 혜택의 신청 메뉴를 확인해 주세요.",
            _ => "공고에 안내된 링크로 연결돼요. 방문·이메일 접수 등 별도 절차가 있는지 신청방법을 확인해 주세요.",
        },
    })
}

/// The worker's link selection for one programme (`program_link_selections`,
/// written by the worker's link resolver). Navigation only: it never enters
/// a checklist's identity (`application_resources::load_with_identity`).
#[derive(Debug, Clone, Default)]
pub enum LinkState {
    /// Not resolved yet, or the resolver is off: the official URL is shown.
    #[default]
    Pending,
    /// Resolved. `selected` is the programme's link; `notice` its best
    /// non-application link. Both `None`: nothing usable, no button.
    Resolved {
        selected: Option<SelectedLink>,
        notice: Option<SelectedLink>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedLink {
    pub url: String,
    /// application | notice | information | portal
    pub kind: String,
    /// direct | login_required
    pub access: String,
}

/// The destination loader: the selection the worker stored, if any.
pub async fn load(
    connection: &mut sqlx::PgConnection,
    program: uuid::Uuid,
) -> Result<LinkState, sqlx::Error> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT s.state, c.url, c.kind, c.access, n.url, n.kind, n.access
         FROM program_link_selections s
         LEFT JOIN program_link_candidates c ON c.id=s.selected_id AND c.program_id=s.program_id
         LEFT JOIN program_link_candidates n ON n.id=s.notice_id AND n.program_id=s.program_id
         WHERE s.program_id=$1",
    )
    .bind(program)
    .fetch_optional(&mut *connection)
    .await?;
    let link = |url: Option<String>, kind: Option<String>, access: Option<String>| {
        Some(SelectedLink {
            url: url?,
            kind: kind?,
            access: access.unwrap_or_else(|| "direct".into()),
        })
    };
    Ok(match row {
        Some((state, url, kind, access, n_url, n_kind, n_access)) => match state.as_str() {
            "none" => LinkState::Resolved {
                selected: None,
                notice: None,
            },
            "selected" => match link(url, kind, access) {
                // The selected row was replaced between two worker steps.
                None => LinkState::Pending,
                selected => LinkState::Resolved {
                    selected,
                    notice: link(n_url, n_kind, n_access),
                },
            },
            _ => LinkState::Pending,
        },
        None => LinkState::Pending,
    })
}

/// The label and kind the client shows for a resolver link (design 5.5),
/// chosen here from the link's kind and access; the client opens only
/// `url` and checks it against `origin`.
pub fn from_link(link: &SelectedLink) -> Option<Destination> {
    let mut result = destination(&link.url, "link_resolver")?;
    let gov24 = matches!(result.platform.as_str(), "government24");
    let (kind, label, explanation) = match (link.kind.as_str(), link.access.as_str()) {
        ("application", "login_required") => (
            "login_required",
            "로그인 후 신청하기",
            "기관 로그인·본인인증 후 신청 화면으로 이어져요.",
        ),
        ("application", _) => (
            "application",
            "신청 페이지로 바로 가기",
            "신청 화면으로 연결돼요. 입력·동의·최종 제출은 기관 화면에서 직접 진행해 주세요.",
        ),
        ("notice", _) => (
            "unverified_page",
            "공고 안내 보기",
            "공고 안내 페이지로 연결돼요. 방문·이메일 접수 등 별도 절차가 있는지 신청방법을 확인해 주세요.",
        ),
        ("information", _) if gov24 => (
            "information",
            "정부24에서 신청 안내 보기",
            "정부24 안내 페이지로 연결돼요. 안내된 신청 방법에 따라 신청을 이어가세요.",
        ),
        ("information", _) => (
            "information",
            "공고 안내 보기",
            "공고 안내 페이지로 연결돼요. 안내된 접수 방식에 따라 신청을 이어가세요.",
        ),
        ("portal", _) => (
            "portal",
            "기관 사이트에서 찾기",
            "기관 사이트로 연결돼요. 해당 혜택의 신청 메뉴를 확인해 주세요.",
        ),
        _ => return None,
    };
    result.kind = kind;
    result.label = label;
    result.explanation = explanation;
    Some(result)
}

/// The resolver's (application link, notice link) for a programme.
/// `Pending` keeps today's behaviour: the official URL as a notice link.
pub fn fallback(
    official: Option<&str>,
    state: &LinkState,
) -> (Option<Destination>, Option<Destination>) {
    match state {
        LinkState::Pending => (None, resolve(official)),
        LinkState::Resolved { selected, notice } => {
            let application = selected
                .as_ref()
                .filter(|l| l.kind == "application")
                .and_then(from_link);
            // No notice link: offline channels get none rather than an
            // application label they cannot use.
            (application, notice.as_ref().and_then(from_link))
        }
    }
}

/// The programme link the agent's tools show (issue links, the official-page
/// check): the URL the programme's own button would open ([`fallback`]: the
/// resolver's application link, else its notice link; the official URL only
/// while the resolver has not decided). A programme whose links all failed
/// shows none.
pub fn shown_url(official: Option<&str>, state: &LinkState) -> Option<String> {
    let (application, notice) = fallback(official, state);
    // `destination` already admitted only public HTTPS; the URL keeps its
    // query and fragment (they may name the form).
    application.or(notice).map(|d| d.url)
}

/// Only service-selected, current, reviewed evidence can use a direct CTA.
pub fn verified(resource: &super::application_resources::PublicResource) -> Option<Destination> {
    let mut result = destination(&resource.url, "reviewed_application")?;
    result.kind = if resource.access == "login_required" {
        "login_required"
    } else {
        "application"
    };
    result.label = if result.kind == "login_required" {
        "로그인 후 신청하기"
    } else {
        "신청하러 가기"
    };
    result.explanation = if result.kind == "login_required" {
        "해당 공고의 신청 경로를 확인했어요. 기관 로그인·본인인증 후 신청을 이어가세요."
    } else {
        "해당 공고의 신청 화면을 확인했어요. 입력·동의·최종 제출은 기관 화면에서 직접 진행해 주세요."
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specific_official_link_keeps_its_navigation_state() {
        let url = "https://provider.example.org/notice?id=42&source=apply#/application";
        let link = resolve(Some(url)).unwrap();
        assert_eq!(link.url, url);
        assert_eq!(link.kind, "unverified_page");
        assert_eq!(link.evidence, "official_url");
        assert_eq!(link.origin, "https://provider.example.org");
    }

    #[test]
    fn information_and_portal_links_are_not_claimed_as_forms() {
        let link = resolve(Some("https://www.uctf.or.kr/?lang=ko")).unwrap();
        assert_eq!(link.kind, "portal");
        let link = resolve(Some("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/42")).unwrap();
        assert_eq!(link.kind, "information");
        assert_eq!(link.platform, "government24");
        let link = resolve(Some(
            "https://www.bokjiro.go.kr/ssis-tbu/twatxa/wlfareInfo/move.do?id=42",
        ))
        .unwrap();
        assert_eq!(link.kind, "information");
        assert_eq!(
            resolve(Some("https://provider.example.org/")).unwrap().kind,
            "portal"
        );
        assert_eq!(
            resolve(Some("https://finlife.fss.or.kr/main/main.do"))
                .unwrap()
                .label,
            "공식 사이트에서 확인"
        );
    }

    #[test]
    fn unsafe_or_malformed_urls_cannot_be_navigation_destinations() {
        for url in [
            "javascript:alert(1)",
            "http://example.org/apply",
            "https://user:pass@example.org/apply",
            "https://localhost/apply",
            "https://127.0.0.1/apply",
            "https://example.org:8443/apply",
            "",
        ] {
            assert!(resolve(Some(url)).is_none(), "{url}");
        }
        assert!(resolve(None).is_none());
    }

    fn link(url: &str, kind: &str, access: &str) -> SelectedLink {
        SelectedLink {
            url: url.into(),
            kind: kind.into(),
            access: access.into(),
        }
    }

    /// Design 5.5: the label comes from kind and access, never from the page.
    #[test]
    fn resolver_links_carry_the_server_chosen_label() {
        let cases = [
            ("https://apply.example.go.kr/aply.do?id=1", "application", "direct", "application", "신청 페이지로 바로 가기"),
            ("https://apply.example.go.kr/aply.do?id=1", "application", "login_required", "login_required", "로그인 후 신청하기"),
            ("https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1", "notice", "direct", "unverified_page", "공고 안내 보기"),
            ("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1", "information", "direct", "information", "정부24에서 신청 안내 보기"),
            ("https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=1", "information", "direct", "information", "공고 안내 보기"),
            ("https://www.kosaf.go.kr/", "portal", "direct", "portal", "기관 사이트에서 찾기"),
        ];
        for (url, kind, access, shown, label) in cases {
            let d = from_link(&link(url, kind, access)).unwrap();
            assert_eq!(
                (d.kind, d.label, d.evidence),
                (shown, label, "link_resolver"),
                "{url}"
            );
            assert_eq!(d.url, url);
        }
        // The navigation rule still applies to a stored link.
        assert!(from_link(&link("http://www.gijang.go.kr/", "portal", "direct")).is_none());
        assert!(from_link(&link("https://x.go.kr/a", "broken", "direct")).is_none());
    }

    #[test]
    fn a_pending_selection_keeps_the_official_url_and_a_resolved_one_replaces_it() {
        let official = Some("https://forms.gle/closed1");
        let (app, notice) = fallback(official, &LinkState::Pending);
        assert!(app.is_none());
        assert_eq!(notice.unwrap().url, "https://forms.gle/closed1");
        let resolved = LinkState::Resolved {
            selected: Some(link(
                "https://apply.example.go.kr/aply.do?id=1",
                "application",
                "direct",
            )),
            notice: Some(link(
                "https://www.wonju.go.kr/www/selectBbsNttView.do?nttNo=1",
                "notice",
                "direct",
            )),
        };
        let (app, notice) = fallback(official, &resolved);
        assert_eq!(app.unwrap().label, "신청 페이지로 바로 가기");
        assert_eq!(notice.unwrap().label, "공고 안내 보기");
        // Broken or missing everywhere: no button, not the dead official URL.
        let none = LinkState::Resolved {
            selected: None,
            notice: None,
        };
        let only_application = LinkState::Resolved {
            selected: Some(link(
                "https://apply.example.go.kr/aply.do?id=1",
                "application",
                "direct",
            )),
            notice: None,
        };
        assert!(fallback(official, &only_application).1.is_none());
        let (app, notice) = fallback(official, &none);
        assert!(app.is_none() && notice.is_none());
    }

    #[test]
    fn platform_matching_never_accepts_lookalike_hosts() {
        let link = resolve(Some("https://www.gov.kr.evil.example/portal/rcvfvrSvc/42")).unwrap();
        assert_ne!(link.platform, "government24");
        assert_eq!(link.kind, "unverified_page");
    }
}

#[cfg(test)]
mod shown_url_tests {
    use super::*;

    fn link(url: &str, kind: &str) -> Option<SelectedLink> {
        Some(SelectedLink {
            url: url.into(),
            kind: kind.into(),
            access: "direct".into(),
        })
    }

    /// The agent's links are the resolver's checked ones: a programme whose
    /// links all failed shows none, not the stale official URL.
    #[test]
    fn the_agent_shows_the_resolvers_link() {
        let official = Some("https://www.city.go.kr/old-list");
        assert_eq!(
            shown_url(official, &LinkState::Pending).as_deref(),
            Some("https://www.city.go.kr/old-list"),
            "unresolved: the official URL"
        );
        let resolved = LinkState::Resolved {
            selected: link("https://apply.city.go.kr/form/1", "application"),
            notice: link("https://www.city.go.kr/notice/1", "notice"),
        };
        assert_eq!(
            shown_url(official, &resolved).as_deref(),
            Some("https://apply.city.go.kr/form/1")
        );
        let notice_only = LinkState::Resolved {
            selected: link("https://www.city.go.kr/portal", "portal"),
            notice: link("https://www.city.go.kr/notice/1", "notice"),
        };
        assert_eq!(
            shown_url(official, &notice_only).as_deref(),
            Some("https://www.city.go.kr/notice/1")
        );
        let broken = LinkState::Resolved {
            selected: None,
            notice: None,
        };
        assert_eq!(
            shown_url(official, &broken),
            None,
            "every link failed: none"
        );
    }
}
