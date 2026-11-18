//! Brand scope on first-party pages (review H5, H6, D6): the audience gate
//! is replaced by a discount cue in the fact's own sentence; every other
//! single-offer guard still applies; an issuer named next to the fact is
//! its sponsor. Fixture: `tests/fixtures/doors_20261110/pages/`.

use crate::corporate_auto::bootstrap::{extract_offer, extract_offer_scoped, BrandScope, Scope};
use crate::corporate_auto::doors::registry::tests::fixture_registry;
use chrono::NaiveDate;
use uuid::Uuid;

const PAGES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/doors_20261110/pages"
);

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
}

fn scope(brand: &str) -> Scope {
    let registry = fixture_registry();
    let brand = registry
        .brands()
        .iter()
        .find(|b| b.name == brand)
        .expect("brand in the fixture registry");
    Scope::Brand(BrandScope::new(
        brand.brand_id,
        &brand.name,
        Uuid::nil(),
        &registry,
    ))
}

fn page(body: &str) -> String {
    // Padding keeps a test page above the script-shell floor.
    let filler =
        "매장 안내와 영업 시간, 고객센터 연락처, 개인정보 처리방침과 이용약관을 확인하세요. "
            .repeat(12);
    format!("<html><head><title>혜택</title></head><body><main>{body}</main><footer><p>{filler}</p></footer></body></html>")
}

/// 공차's own T 멤버십 page: under audience scope nothing is read (no
/// student or youth target); under brand scope the 10% discount is read,
/// the 적립 one is not, and T 멤버십 (SK텔레콤) is its issuer.
#[test]
fn gongcha_t_membership_page_reads_a_sponsored_discount() {
    let html =
        std::fs::read_to_string(format!("{PAGES}/gong-cha.co.kr_skt_membership.html")).unwrap();
    assert_eq!(
        extract_offer(&html, today()).unwrap_err(),
        "no_student_youth_offer"
    );
    let offer = extract_offer_scoped(&html, today(), &scope("공차")).expect("brand scope reads it");
    let kinds: Vec<(&str, String)> = offer
        .facts
        .iter()
        .map(|f| (f.kind, f.value.to_string()))
        .collect();
    assert_eq!(kinds, [("discount_percent", "10".to_string())], "{offer:?}");
    let issuer = offer.issuer.as_ref().expect("the issuer is named");
    assert_eq!(issuer.name, "SK텔레콤");
    assert_eq!(issuer.holder, "telecom_carrier:skt");
    assert!(issuer.line.contains("T 멤버십 할인형"), "{}", issuer.line);
}

/// The negative corpus of brand scope.
#[test]
fn brand_scope_refuses_what_is_not_one_discount_of_the_brand() {
    let brand = scope("공차");
    let read = |body: &str| extract_offer_scoped(&page(body), today(), &brand);
    // No benefit cue: a price is no offer.
    assert_eq!(
        read("<p>버블티 라지 5,500원</p>").unwrap_err(),
        "no_brand_offer"
    );
    // A product page stays refused.
    assert_eq!(
        read("<p>판매가 39,000원</p><p>회원 10% 할인</p>").unwrap_err(),
        "product_listing"
    );
    // A "최대" banner is not a plain value.
    assert_eq!(
        read("<h2>최대 50% 할인 대축제</h2>").unwrap_err(),
        "no_brand_offer"
    );
    // 적립 is not 할인.
    assert_eq!(
        read("<p>멤버십 회원 5% 적립</p>").unwrap_err(),
        "no_brand_offer"
    );
    // Per-amount rates are not flat amounts.
    assert_eq!(
        read("<p>1천원당 100원 할인</p>").unwrap_err(),
        "no_brand_offer"
    );
    // Several values are several offers.
    assert_eq!(
        read("<p>아메리카노 10% 할인</p><p>라떼 20% 할인</p><p>케이크 30% 할인</p>").unwrap_err(),
        "multi_offer_page"
    );
    // Two issuers for one offer.
    assert_eq!(
        read("<p>KB국민카드, 신한카드 결제 시 10% 할인</p>").unwrap_err(),
        "conflicting_issuers"
    );
    // An ended offer stays ended.
    assert_eq!(
        read("<p>10% 할인</p><p>이벤트 기간: 2026.10.01 ~ 2026.10.31</p>").unwrap_err(),
        "offer_ended"
    );
    // One plain discount of the brand's own reads, with no issuer.
    let offer = read("<h2>앱 회원 혜택</h2><p>앱으로 주문하면 1,000원 할인</p>").unwrap();
    assert_eq!(offer.facts[0].kind, "krw");
    assert_eq!(offer.facts[0].value, serde_json::json!(1000));
    assert!(offer.issuer.is_none());
}

/// Audience scope is unchanged: a student page reads as before, and brand
/// scope does not read link text.
#[test]
fn audience_scope_is_unchanged_and_links_are_never_facts() {
    let student = page(
        "<p>대학생은 Notion Plus 요금제를 무료로 이용할 수 있습니다. 재학 중 계속 무료입니다.</p>",
    );
    assert_eq!(
        extract_offer(&student, today()),
        extract_offer_scoped(&student, today(), &Scope::Audience)
    );
    let linked = page("<p><a href=\"/x\">10% 할인 쿠폰 받기</a></p>");
    assert_eq!(
        extract_offer_scoped(&linked, today(), &scope("공차")).unwrap_err(),
        "no_brand_offer"
    );
}

fn read(body: &str) -> Result<crate::corporate_auto::bootstrap::Offer, &'static str> {
    extract_offer_scoped(&page(body), today(), &scope("도미노피자"))
}

/// Code review C-2: issuer names bind only as whole tokens, a longer brand
/// name wins, an issuer named away from the offer refuses it, and two
/// values for one issuer are two offers.
#[test]
fn brand_scope_binds_whole_issuer_tokens_and_fails_closed() {
    for (body, expected) in [
        ("<p>KTX 승차권 소지 고객 10% 할인</p>", None),
        ("<p>토스트 세트 10% 할인</p>", None),
        ("<p>KT 고객 10% 할인</p>", Some("KT")),
        ("<p>KB국민카드로 결제 시 20% 할인</p>", Some("KB국민카드")),
    ] {
        let offer = read(body).unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(
            offer.issuer.as_ref().map(|i| i.name.as_str()),
            expected,
            "{body}"
        );
    }
    // 토스뱅크 is a brand of its own, never the pay app 토스.
    assert_eq!(
        read("<p>토스뱅크 체크카드로 결제 시 5% 할인</p>")
            .unwrap()
            .issuer,
        None
    );
    // An issuer named in a heading paragraphs away: refused, never the
    // brand's own offer.
    assert_eq!(
        read("<h2>KB국민카드 11월 이벤트</h2><p>매장에서 즐기는 따뜻한 겨울 메뉴를 소개합니다. 새로운 메뉴를 만나 보세요.</p><p>방문포장 20% 할인</p>")
            .unwrap_err(),
        "issuer_unbound"
    );
    // A tier table: two values for one issuer.
    assert_eq!(
        read("<p>T멤버십 VIP 30% 할인</p><p>T멤버십 골드/실버 20% 할인</p>").unwrap_err(),
        "multi_offer_page"
    );
}

/// Code review H-6: the title is the fact's sentence; different subjects
/// are several offers; a yearless period refuses on every page.
#[test]
fn brand_scope_keeps_the_condition_and_refuses_what_it_cannot_tell_apart() {
    let offer = read("<p>신규 가입 후 첫 구매 시 10% 할인</p>").unwrap();
    assert_eq!(
        crate::corporate_auto::publish::offer_title("도미노피자", &offer).unwrap(),
        "도미노피자 신규 가입 후 첫 구매 시 10% 할인"
    );
    assert_eq!(
        read("<ul><li>아메리카노 10% 할인</li><li>라떼 10% 할인</li><li>케이크 10% 할인</li></ul>")
            .unwrap_err(),
        "multi_offer_page"
    );
    assert_eq!(
        read("<p>방문포장 20% 할인</p><p>이벤트 기간: 10월 1일 ~ 10월 31일</p>").unwrap_err(),
        "period_without_year"
    );
}

/// Verify finding F6: each of `dated_article`'s four refusals.
#[test]
fn a_list_child_must_be_an_article_of_now() {
    use crate::corporate_auto::bootstrap::dated_article;
    let d = |m: u32, day: u32| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
    let published = |date: &str| {
        format!("<html><head><meta property=\"article:published_time\" content=\"{date}\"></head><body></body></html>")
    };
    assert_eq!(
        dated_article("<html><body></body></html>", Some(d(11, 30)), today()),
        Err("article_undated")
    );
    assert_eq!(
        dated_article(&published("2026-09-01"), Some(d(11, 30)), today()),
        Err("article_too_old")
    );
    assert_eq!(
        dated_article(&published("2026-11-01"), None, today()),
        Err("article_without_end")
    );
    assert_eq!(
        dated_article(&published("2026-11-01"), Some(d(11, 10)), today()),
        Err("article_end_implausible"),
        "ended"
    );
    assert_eq!(
        dated_article(
            &published("2026-11-01"),
            Some(NaiveDate::from_ymd_opt(2027, 3, 1).unwrap()),
            today()
        ),
        Err("article_end_implausible"),
        "too long after publication"
    );
    assert_eq!(
        dated_article(&published("2026-11-01"), Some(d(11, 30)), today()),
        Ok(())
    );
}
