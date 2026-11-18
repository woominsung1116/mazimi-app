//! Readers against real pages saved from production hosts on 2026-10-31
//! (`crates/worker/tests/fixtures/apply_links`, session tokens redacted).
use super::readers::{self, Entry};
use super::structure::{self, Step};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/apply_links/", $name))
    };
}

const SEOUL_URL: &str = "https://youth.seoul.go.kr/youthConts.do?key=2310100075&sc_pbancSeCd=001&sc_bbsStngSn=2212200001&sc_bbsCtgrySn=2310200002&sc_qnaCtgryCd=&sc_faqCtgryCd=007";

fn link(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Link { url, .. } => Some(url),
        _ => None,
    }
}

/// Counterexample: the seeded discovery (`discovery::links`) takes the
/// site menu's "신청하기" (to the site's main page) for an application,
/// which is how unverified application candidates pile up today.
#[test]
fn seeded_discovery_takes_site_menus_for_applications() {
    let found =
        super::super::discovery::links(SEOUL_URL, fixture!("seoul_youth_policy.html"), None);
    let applications: Vec<&str> = found
        .iter()
        .filter(|c| c.kind == "application")
        .map(|c| c.url.as_str())
        .collect();
    assert_eq!(
        applications.first(),
        Some(&"https://youth.seoul.go.kr/mainB.do")
    );
}

#[test]
fn seoul_policy_page_names_its_own_content_button() {
    let entry = readers::generic(SEOUL_URL, fixture!("seoul_youth_policy.html"));
    assert_eq!(
        link(&entry),
        Some("https://youth.seoul.go.kr/loanCrdtRcvr/step1.do?key=2310100075"),
        "{entry:?}"
    );
    // The target is the first step of the application: a Y/N questionnaire
    // form next to the site search box.
    assert_eq!(
        structure::step(fixture!("seoul_youth_apply_step.html")),
        Step::ApplicationForm
    );
    // The policy page itself is not an application step.
    assert_eq!(
        structure::step(fixture!("seoul_youth_policy.html")),
        Step::Other
    );
}

#[test]
fn pages_whose_only_application_links_are_menus_offer_none() {
    for (url, html) in [
        (
            "https://youth.incheon.go.kr/youthpolicy/youthPolicyInfoDetail.do?poly_seq=312",
            fixture!("incheon_youth_policy.html"),
        ),
        (
            "https://www.busan.go.kr/expdetail/view?schYear=2025&schDbiz=626000020253008E",
            fixture!("busan_policy_detail.html"),
        ),
        (
            "https://www.usc.go.kr/youth/page.do?mnu_uid=2019&",
            fixture!("uiseong_youth_page.html"),
        ),
        (
            "https://youth.chungnam.go.kr/web/main/customSupp/M040-06/view?bizId=A20260309CT000000000003043",
            fixture!("chungnam_youth_policy.html"),
        ),
    ] {
        assert_eq!(
            readers::generic(url, html),
            Entry::None("no_apply_element"),
            "{url}"
        );
    }
}

#[test]
fn bokjiro_reads_the_embedded_online_application_target() {
    let page = "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00000078&wlfareInfoReldBztpCd=01";
    let entry = readers::bokjiro(page, fixture!("bokjiro_national_online.html"));
    assert_eq!(
        link(&entry),
        Some("https://www.bokjiro.go.kr/ssis-tbu/twoaa/aplySrvChc/selectCvlcptServChs.do?menuGb=twoaa&srvGb=020"),
        "{entry:?}"
    );
    // A local service that offers no online application says so.
    let local = "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00006018&wlfareInfoReldBztpCd=02";
    assert_eq!(
        readers::bokjiro(local, fixture!("bokjiro_local_offline.html")),
        Entry::None("bokjiro_no_online_application")
    );
    // A page without the record (a changed layout) is not read as "no".
    assert_eq!(
        readers::bokjiro(page, "<html><title>복지로</title></html>"),
        Entry::None("bokjiro_fields_missing")
    );
}

/// Why the 온통청년 reader uses `aplyUrlAddr`: the page's button has no
/// href, its click handler opens that field. If the page changes, this
/// fails and the reader must be revisited.
#[test]
fn youthcenter_button_opens_the_policy_apply_url() {
    let html = fixture!("youthcenter_detail.html");
    let handler = html
        .find("$('.gourl').YTHIPon(\"click\"")
        .expect("신청바로가기 click handler");
    assert!(html[handler..handler + 300].contains("const url = plcyList.aplyUrlAddr;"));
    assert!(html
        .contains(r#"<a href="javascript:;" class="btn-type03 col-sm-12 gourl">신청바로가기</a>"#));
    // So the structural reader finds no target on the page itself.
    let page = "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/20260918005400113439";
    assert_eq!(
        readers::generic(page, html),
        Entry::None("no_apply_element")
    );
}

/// Why the 정부24 reader uses `serviceDetail`: the server HTML is a shell
/// whose state carries no service data (it loads from robots-disallowed
/// `/api/...`), and its 신청 links are site menus.
#[test]
fn gov24_page_shell_has_no_service_data() {
    let html = fixture!("gov24_benefit_detail.html");
    assert!(html.contains("__NUXT_DATA__"));
    assert!(
        !html.contains("319000000167"),
        "service id in the server HTML"
    );
    assert!(html.contains(r#"portalSvcApiUrl:"/api/ptl/svcapi/v2.0""#));
    let page = "https://plus.gov.kr/portal/benefitV2/benefitTotalSrvcList/benefitSrvcDtl/?svcSeq=2147905&bnefType=all&svcId=319000000167";
    assert_eq!(
        readers::generic(page, html),
        Entry::None("no_apply_element")
    );
}

/// Precision check of the 710-programme sample (2026-10-31): two pages
/// were taken for application forms. A notice whose only form is the
/// site search (`<form id="search_form">`, field `qsrchSw`) and a
/// membership sign-up consent page (seven checkboxes) are not application
/// steps.
#[test]
fn search_and_consent_forms_are_not_application_forms() {
    assert_eq!(
        structure::step(fixture!("busanjob_notice_view.html")),
        Step::Other
    );
    assert_eq!(
        structure::step(fixture!("foodvoucher_join_agree.html")),
        Step::Other
    );
    // Radio questionnaires remain application forms (Seoul step 1).
    assert_eq!(
        structure::step(fixture!("seoul_youth_apply_step.html")),
        Step::ApplicationForm
    );
}

/// Holdout precision check (2026-10-31): a 고용24 programme list whose
/// filter form (two selects, two dates, a status select) was taken for an
/// application form. A form under a search or listing title searches.
#[test]
fn a_filter_form_on_a_list_page_is_not_an_application_form() {
    assert_eq!(
        structure::step(fixture!("work24_programme_list.html")),
        Step::Other
    );
}
