//! The hub row reader on the saved hub pages (2026-11-10) and its negative
//! corpus. Fixtures: `tests/fixtures/doors_20261110/hubs/`.

use super::registry::tests::fixture_registry;
use super::registry::Registry;
use super::rows::{read_hub, row_period, HubRead, RowOffer, TierReading, ValueKind};
use chrono::NaiveDate;
use uuid::Uuid;

const HUBS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/doors_20261110/hubs"
);

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{HUBS}/{name}")).expect("hub fixture")
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
}

fn id(registry: &Registry, name: &str) -> Uuid {
    registry
        .brands()
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("{name} in the fixture registry"))
        .brand_id
}

fn offer<'a>(read: &'a HubRead, registry: &Registry, name: &str) -> Option<&'a RowOffer> {
    let brand = id(registry, name);
    read.offers.iter().find(|o| o.brand_id == brand)
}

fn rejected(read: &HubRead, registry: &Registry, name: &str) -> Vec<&'static str> {
    let brand = id(registry, name);
    read.rejected_brands
        .iter()
        .filter(|(b, _)| *b == brand)
        .map(|(_, r)| *r)
        .collect()
}

fn skt() -> (Registry, HubRead) {
    let registry = fixture_registry();
    let grantor = id(&registry, "SK텔레콤");
    let read = read_hub(
        &fixture("sktmembership_list-tab1.html"),
        &registry,
        grantor,
        today(),
    );
    (registry, read)
}

fn kb() -> (Registry, HubRead) {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    let read = read_hub(
        &fixture("kbcard_brand_discounts.html"),
        &registry,
        grantor,
        today(),
    );
    (registry, read)
}

/// T 멤버십: rows are the partner boxes; tier letters are hidden labels
/// that group the values (review H1). Only what every tier gets is
/// accepted, the lowest when tiers differ.
#[test]
fn t_membership_rows_are_read_by_tier() {
    let (registry, read) = skt();
    assert!(read.listing, "{read:?}");
    assert!(read.rows_seen >= 15, "{}", read.rows_seen);
    assert!(
        read.tiered_rows >= 12,
        "most rows are tiered: {}",
        read.tiered_rows
    );
    // 공차: VGS 10% — every tier the same.
    let gongcha = offer(&read, &registry, "공차").expect("공차 accepted");
    assert_eq!(
        (gongcha.kind, gongcha.value.as_str()),
        (ValueKind::Percent, "10")
    );
    assert_eq!(gongcha.tiers, TierReading::EveryTierSame);
    assert!(!gongcha.upper_bound);
    // 배스킨라빈스: V 50%, G/S 30% — the lowest, labelled.
    let baskin = offer(&read, &registry, "배스킨라빈스").expect("배스킨 accepted");
    assert_eq!(baskin.value, "30");
    assert_eq!(baskin.tiers, TierReading::EveryTierMinimum);
    assert_eq!(
        baskin.title("SK텔레콤"),
        "SK텔레콤 · 배스킨라빈스 할인형 싱글레귤러 30% 할인(1,290원) (등급별 최솟값)"
    );
    // The hidden tier letters, the badges, the other tier, the 적립형 group
    // and the "자세히 보기" button never enter the line; the membership type
    // heading does (review H-2, M-2).
    assert_eq!(
        baskin.quote,
        "배스킨라빈스 할인형 싱글레귤러 30% 할인(1,290원)"
    );
    // CGV: "최대 4,000원" for every tier, kept as an upper bound (review M7).
    let cgv = offer(&read, &registry, "CGV").expect("CGV accepted");
    assert_eq!((cgv.kind, cgv.value.as_str()), (ValueKind::Won, "4000"));
    assert!(cgv.upper_bound);
    assert_eq!(cgv.value_text(), "최대 4,000원");
    // A per-amount rate is not a flat discount: 1천원당 100원 is not 100원.
    for name in ["CU", "파리바게뜨", "뚜레쥬르", "세븐일레븐", "롯데리아"] {
        assert!(offer(&read, &registry, name).is_none(), "{name}");
        assert!(
            rejected(&read, &registry, name).contains(&"rate_per_amount"),
            "{name}: {:?}",
            rejected(&read, &registry, name)
        );
    }
    // 이마트: "짝수월 7%, 홀수월 3%" states two values for one tier.
    assert!(offer(&read, &registry, "이마트").is_none());
    // 메가MGC: a tier (L) the discount lines never name gets nothing.
    assert!(offer(&read, &registry, "메가MGC커피").is_none());
    assert_eq!(
        rejected(&read, &registry, "메가MGC커피"),
        ["not_every_tier"]
    );
    // No 적립 statement is read as a discount, and nothing binds the hub.
    for o in &read.offers {
        assert!(!o.quote.contains("적립"), "{o:?}");
        assert_ne!(o.brand_id, id(&registry, "SK텔레콤"));
    }
}

/// KB국민카드 브랜드 할인: dated rows, sub-brands never bind their parent
/// (review H2), "최대" kept verbatim, future and conflicting rows refused.
#[test]
fn kb_rows_bind_whole_names_and_read_their_period() {
    let (registry, read) = kb();
    assert!(read.listing);
    assert_eq!(read.rows_seen, 46);
    let coupang = offer(&read, &registry, "쿠팡").expect("쿠팡 accepted");
    assert_eq!((coupang.value.as_str(), coupang.upper_bound), ("50", true));
    assert_eq!(coupang.value_text(), "최대 50%");
    assert_eq!(coupang.ends, NaiveDate::from_ymd_opt(2026, 11, 30));
    assert_eq!(coupang.starts, NaiveDate::from_ymd_opt(2026, 11, 1));
    assert_eq!(coupang.quote, "쿠팡 최대 50% 즉시할인");
    let jinair = offer(&read, &registry, "진에어").expect("진에어 accepted");
    assert_eq!(
        (jinair.kind, jinair.value.as_str()),
        (ValueKind::Won, "50000")
    );
    // LGE.COM binds LG전자 through its alias; the line prints the name as
    // the row does (review M-4: the online store, not every LG전자 shop).
    let lge = offer(&read, &registry, "LG전자").expect("LGE.COM row");
    assert_eq!(lge.quote, "LGE.COM 특정품목 7% 청구할인");
    // The restriction stays in the title with its value.
    assert_eq!(
        lge.title("KB국민카드"),
        "KB국민카드 · LGE.COM 특정품목 7% 청구할인"
    );
    // SSG닷컴 starts 11.19: not yet.
    assert_eq!(rejected(&read, &registry, "SSG닷컴"), ["not_started"]);
    // 컬리 (1만 2천원 / 20만원) and NOL (4% / 최대 20%): two values.
    assert!(offer(&read, &registry, "컬리").is_none());
    assert!(rejected(&read, &registry, "컬리").contains(&"conflicting_values"));
    assert!(offer(&read, &registry, "야놀자").is_none());
    // 상품권 증정 and 행사상품 즉시할인 are not discounts with a value.
    assert!(offer(&read, &registry, "AK플라자").is_none());
    assert!(offer(&read, &registry, "롯데백화점").is_none());
    assert!(offer(&read, &registry, "이마트").is_none());
    assert!(offer(&read, &registry, "롯데마트").is_none());
    // Review H2: "이마트 에브리데이", "홈플러스 익스프레스" are not 이마트 or 홈플러스.
    assert!(!read.matched_brands().contains(&id(&registry, "홈플러스")));
    assert!(rejected(&read, &registry, "이마트")
        .iter()
        .all(|r| *r == "no_value"));
}

/// The period never enters the offer key: next month's identical row is
/// the same offer (review H8).
#[test]
fn an_identical_row_with_a_new_period_keeps_its_offer_key() {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    let html = fixture("kbcard_brand_discounts.html");
    let now = read_hub(&html, &registry, grantor, today());
    let next = html.replace("2026.11.01 ~ 2026.11.30", "2026.12.01 ~ 2026.12.31");
    let december = NaiveDate::from_ymd_opt(2026, 12, 2).unwrap();
    let later = read_hub(&next, &registry, grantor, december);
    let key = |read: &HubRead| offer(read, &registry, "쿠팡").map(|o| o.offer_key.clone());
    assert!(key(&now).is_some());
    assert_eq!(key(&now), key(&later));
    assert_eq!(
        offer(&later, &registry, "쿠팡").unwrap().ends,
        NaiveDate::from_ymd_opt(2026, 12, 31)
    );
}

/// A redesigned list (new tags and classes, same rows) reads the same.
#[test]
fn a_redesigned_listing_reads_the_same_rows() {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    let html = fixture("kbcard_brand_discounts.html");
    let redesigned = html
        .replace("<li class=\"col c6\">", "<div class=\"card-item\">")
        .replace("</li>", "</div>")
        .replace("class=\"store\"", "class=\"brand-name\"")
        .replace("class=\"subject\"", "class=\"benefit\"");
    let before = read_hub(&html, &registry, grantor, today());
    let after = read_hub(&redesigned, &registry, grantor, today());
    let keys = |read: &HubRead| {
        let mut keys: Vec<String> = read.offers.iter().map(|o| o.offer_key.clone()).collect();
        keys.sort();
        keys
    };
    assert!(!before.offers.is_empty());
    assert_eq!(keys(&before), keys(&after));
}

/// Prose, a price list, banners: no listing, nothing accepted.
#[test]
fn pages_that_are_not_listings_are_unrecognised() {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    let prose = "<html><body><p>쿠팡에서 최대 50% 할인을 받으세요.</p><p>11번가 10% 할인.</p></body></html>";
    let read = read_hub(prose, &registry, grantor, today());
    assert!(!read.listing && read.offers.is_empty());
    let nav: String = (0..8)
        .map(|i| format!("<li class=\"menu\"><a href=\"/m{i}\">메뉴 {i}</a></li>"))
        .collect();
    let read = read_hub(&format!("<ul>{nav}</ul>"), &registry, grantor, today());
    assert!(!read.listing);
}

/// The negative corpus of brand binding and values (review H2, M7, D6).
#[test]
fn negative_rows_are_refused_for_their_reason() {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    let row = |store: &str, subject: &str, date: &str| {
        format!(
            "<li class=\"r\"><span class=\"store\">{store}</span><span class=\"subject\">{subject}</span><span class=\"date\">{date}</span></li>"
        )
    };
    let filler: String = (0..5)
        .map(|i| row(&format!("기타{i}"), "5% 할인", "2026.11.01 ~ 2026.11.30"))
        .collect();
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "쿠팡",
            "5~10% 할인",
            "2026.11.01 ~ 2026.11.30",
            "range_value",
        ),
        (
            "쿠팡",
            "1천원당 100원 할인",
            "2026.11.01 ~ 2026.11.30",
            "rate_per_amount",
        ),
        (
            "쿠팡",
            "5% 적립",
            "2026.11.01 ~ 2026.11.30",
            "earn_not_read",
        ),
        (
            "쿠팡",
            "최대 3만원 캐시백",
            "2026.11.01 ~ 2026.11.30",
            "kind_not_read",
        ),
        ("쿠팡", "10% 할인", "11.01 ~ 11.30", "period_without_year"),
        ("쿠팡", "10% 할인", "2026.10.01 ~ 2026.10.31", "period_past"),
        ("쿠팡", "10% 할인", "~31-01-01", "period_year_implausible"),
        (
            "쿠팡",
            "판매가 39,000원",
            "2026.11.01 ~ 2026.11.30",
            "no_discount_cue",
        ),
    ];
    for (store, subject, date, reason) in cases {
        let html = format!("<ul>{filler}{}</ul>", row(store, subject, date));
        let read = read_hub(&html, &registry, grantor, today());
        assert!(read.listing, "{subject}");
        assert_eq!(
            rejected(&read, &registry, store),
            [*reason],
            "{subject} {date}"
        );
        assert!(read.offers.is_empty(), "{subject}");
    }
    // A sub-brand name and a logo naming another brand.
    let html = format!(
        "<ul>{filler}{}<li class=\"r\"><span class=\"store\">쿠팡</span><img alt=\"11번가\"><span class=\"subject\">10% 할인</span></li></ul>",
        row("이마트 에브리데이", "10% 할인", "2026.11.01 ~ 2026.11.30")
    );
    let read = read_hub(&html, &registry, grantor, today());
    assert!(!read.matched_brands().contains(&id(&registry, "이마트")));
    assert_eq!(rejected(&read, &registry, "쿠팡"), ["logo_disagrees"]);
    // Two brands in one row.
    let html = format!(
        "<ul>{filler}<li class=\"r\"><span>쿠팡</span><span>11번가</span><span>10% 할인</span></li></ul>"
    );
    let read = read_hub(&html, &registry, grantor, today());
    assert_eq!(read.rejected.get("ambiguous_brand"), Some(&1));
}

#[test]
fn two_digit_years_are_read_only_within_the_window() {
    let today = today();
    assert_eq!(
        row_period("~26-11-30", today).map(|p| p.map(|p| (p.starts, p.ends))),
        Ok(Some((None, NaiveDate::from_ymd_opt(2026, 11, 30).unwrap())))
    );
    assert_eq!(
        row_period("~29-01-01", today).map(|p| p.is_some()),
        Err("period_year_implausible")
    );
    assert_eq!(
        row_period("~24-12-31", today),
        Err("period_year_implausible")
    );
    assert_eq!(row_period("혜택 안내", today), Ok(None));
}

/// PAYCO's coupon table: the merchant cell binds, the value cell holds the
/// amount, the 유형 cell says 할인 (PAYCO itself stays behind its own flag).
#[test]
fn payco_rows_bind_the_merchant_cell() {
    let registry = fixture_registry();
    let grantor = id(&registry, "PAYCO");
    let read = read_hub(
        &fixture("payco_coupon_list.html"),
        &registry,
        grantor,
        today(),
    );
    assert!(read.listing, "{:?}", read.rejected);
    let musinsa = offer(&read, &registry, "무신사").unwrap_or_else(|| {
        panic!(
            "무신사 accepted: {:?} / {:?}",
            rejected(&read, &registry, "무신사"),
            read.rejected
        )
    });
    assert_eq!(
        (musinsa.kind, musinsa.value.as_str()),
        (ValueKind::Won, "10000")
    );
    assert_eq!(musinsa.ends, NaiveDate::from_ymd_opt(2026, 11, 30));
}

/// Review H2's named case: KB's "이마트 에브리데이" row is not 이마트's.
#[test]
fn kb_emart_everyday_is_not_emart() {
    let (registry, read) = kb();
    let emart = id(&registry, "이마트");
    // KB lists 이마트 itself once ("행사상품 즉시할인", no value) and
    // 이마트 에브리데이 once; only the first is 이마트's, and neither is read.
    assert_eq!(
        read.rejected_brands
            .iter()
            .filter(|(b, _)| *b == emart)
            .count(),
        1
    );
    assert!(offer(&read, &registry, "이마트").is_none());
}

/// Every accepted line on the three saved hub pages, pinned (the review's
/// offers table, §3, reproduced after H-2, H-5, H-7, M-2..M-5).
#[test]
fn the_saved_hub_pages_accept_exactly_these_lines() {
    let registry = fixture_registry();
    let mut lines: Vec<String> = Vec::new();
    for (file, grantor) in [
        ("sktmembership_list-tab1.html", "SK텔레콤"),
        ("kbcard_brand_discounts.html", "KB국민카드"),
        ("payco_coupon_list.html", "PAYCO"),
    ] {
        let read = read_hub(&fixture(file), &registry, id(&registry, grantor), today());
        for o in &read.offers {
            lines.push(format!(
                "{} | {:?} | {:?}",
                o.title(grantor),
                o.starts,
                o.ends
            ));
        }
    }
    let expected: &[&str] = &[
        "SK텔레콤 · 배스킨라빈스 할인형 싱글레귤러 30% 할인(1,290원) (등급별 최솟값) | None | None",
        "SK텔레콤 · 파파존스피자 할인형 15% 할인 (등급별 최솟값) | None | None",
        "SK텔레콤 · 폴 바셋 할인형 시그니처 커피 4종(사이즈 무관) 구매 시 10% 할인 | None | None",
        "SK텔레콤 · VIPS 할인형 5% 할인 (등급별 최솟값) | None | None",
        "SK텔레콤 · CGV 할인형 영화 관람권(11,000원 이상) 예매 시 최대 4,000원 할인 | None | None",
        "SK텔레콤 · 던킨 할인형 5% 할인(1일 1회) (등급별 최솟값) | None | None",
        "SK텔레콤 · 아웃백 스테이크하우스 할인형 5% 할인 (1일 1회, 일 최대 10,000원 / 월 최대 4회 이용 가능) (등급별 최솟값) | None | None",
        "SK텔레콤 · 11번가 할인형 상시 최대 11% 할인 | None | None",
        "SK텔레콤 · 피자헛 할인형 20% 할인 (등급별 최솟값) | None | None",
        "SK텔레콤 · 도미노피자 할인형 20% 할인 (등급별 최솟값) | None | None",
        "SK텔레콤 · 공차 할인형 10% 할인 | None | None",
        "KB국민카드 · 쿠팡 최대 50% 즉시할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · CJ온스타일 최대 10％ 즉시할인 | Some(2026-11-01) | Some(2026-11-14)",
        "KB국민카드 · 옥션 최대 7% 즉시할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · LGE.COM 특정품목 7% 청구할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · G마켓 최대 12% 즉시할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · SAMSUNG.COM 특정품목 7% 청구할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · 진에어 최대 5만원 할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · 29CM 최대 2만원 즉시할인 | Some(2026-11-09) | Some(2026-11-30)",
        "KB국민카드 · 11번가 최대 15% 즉시할인 | Some(2026-11-01) | Some(2026-11-30)",
        "KB국민카드 · 롯데ON 최대 15% 즉시할인 | Some(2026-11-01) | Some(2026-11-30)",
        "PAYCO · 모던하우스 11.5% 전 고객 할인 리빙, 생필품 등 침구, 인테리어소품, 가구 페이코 포인트 쿠폰 | None | Some(2026-11-30)",
        "PAYCO · 마켓컬리 1,000원 전 고객 할인 푸드, 생필품 등 신선식품, 가공식품, 간편식 페이코 쿠폰 | None | Some(2026-11-30)",
        "PAYCO · 영풍문고 1,000원 전 고객 할인 여행/콘텐츠 도서, 책 페이코 포인트 쿠폰 | None | Some(2026-11-30)",
        "PAYCO · 무신사 10,000원 가맹점 첫결제 고객 할인 패션 패션, 잡화, 남여의류, 뷰티 페이코 쿠폰 | None | Some(2026-11-30)",
    ];
    assert_eq!(lines, expected, "{lines:#?}");
}

fn kb_row(store: &str, subject: &str, date: &str, extra: &str) -> String {
    format!(
        "<li><span class=\"store\">{store}</span><span class=\"subject\">{subject}</span><span class=\"date\">{date}</span>{extra}</li>"
    )
}

const FIVE: [&str; 5] = ["쿠팡", "옥션", "G마켓", "11번가", "진에어"];

fn kb_page(rows: &[String]) -> HubRead {
    let registry = fixture_registry();
    let grantor = id(&registry, "KB국민카드");
    read_hub(
        &format!("<html><body><ul>{}</ul></body></html>", rows.concat()),
        &registry,
        grantor,
        today(),
    )
}

/// Review H-1: every date token of a row must be read as its period.
#[test]
fn every_date_token_belongs_to_the_period() {
    let today = today();
    let ends = |text: &str| row_period(text, today).map(|p| p.map(|p| (p.starts, p.ends)));
    let d = |m: u32, day: u32| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
    assert_eq!(
        ends("26.12.01 ~ 26.12.31"),
        Ok(Some((Some(d(12, 1)), d(12, 31))))
    );
    assert_eq!(
        ends("2026.12.01(화) ~ 2026.12.31(목)"),
        Ok(Some((Some(d(12, 1)), d(12, 31))))
    );
    assert_eq!(
        ends("2026.10.01 - 2026.10.31"),
        Ok(Some((Some(d(10, 1)), d(10, 31))))
    );
    assert_eq!(
        ends("2026년 10월 1일 ~ 2026년 10월 31일"),
        Ok(Some((Some(d(10, 1)), d(10, 31))))
    );
    assert_eq!(ends("2026.11.30까지"), Ok(Some((None, d(11, 30)))));
    for yearless in [
        "12.01(화) ~ 12.31(목)",
        "11월 1일 ~ 11월 30일",
        "~11/30",
        "11.30까지",
    ] {
        assert_eq!(ends(yearless), Err("period_without_year"), "{yearless}");
    }
    assert_eq!(ends("2026.11.01 오픈"), Err("period_unparsed"));
    assert_eq!(
        ends("2026.11.01 ~ 2026.11.15, 2026.11.20 ~ 2026.11.30"),
        Err("period_unparsed")
    );
    // Values are not dates.
    assert_eq!(ends("11.5% 할인, 3.5만원 이상"), Ok(None));
    // A row: a period that has not started is refused, never shown as now.
    let mut rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5% 할인", "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    rows[0] = kb_row("쿠팡", "5% 할인", "26.12.01 ~ 26.12.31", "");
    rows[1] = kb_row("옥션", "5% 할인", "2026년 10월 1일 ~ 2026년 10월 31일", "");
    rows[2] = kb_row("G마켓", "5% 할인", "12.01(화) ~ 12.31(목)", "");
    let read = kb_page(&rows);
    let registry = fixture_registry();
    assert_eq!(rejected(&read, &registry, "쿠팡"), ["not_started"]);
    assert_eq!(rejected(&read, &registry, "옥션"), ["period_past"]);
    assert_eq!(rejected(&read, &registry, "G마켓"), ["period_without_year"]);
    assert_eq!(read.offers.len(), 2);
}

/// Review H-5: a brand binds only at the position where most rows name
/// one; a category or product cell naming another brand never binds.
#[test]
fn a_brand_binds_at_the_row_name_position() {
    let registry = fixture_registry();
    let mut rows: Vec<String> = FIVE
        .iter()
        .map(|s| {
            kb_row(
                s,
                "5% 할인",
                "2026.11.01 ~ 2026.11.30",
                "<span class=\"cat\">패션</span>",
            )
        })
        .collect();
    rows.push(kb_row(
        "닥터자르트",
        "20% 할인",
        "2026.11.01 ~ 2026.11.30",
        "<span class=\"cat\">자주</span>",
    ));
    let read = kb_page(&rows);
    assert_eq!(read.offers.len(), 5, "{:?}", read.rejected);
    assert!(offer(&read, &registry, "자주").is_none());
    assert!(rejected(&read, &registry, "자주").is_empty());
}

/// Review H-7: a menu of values with no brand never outranks the brand
/// list; a list that binds no brand is not a listing.
#[test]
fn the_listing_is_the_group_that_names_brands() {
    let registry = fixture_registry();
    let menu: String = (0..8)
        .map(|i| {
            format!(
                "<li class=\"menu\"><a href=\"/m{i}\">기획전 {i} 최대 {}0% 할인</a></li>",
                i + 1
            )
        })
        .collect();
    let list: String = FIVE
        .iter()
        .chain(["29CM"].iter())
        .map(|s| kb_row(s, "5% 할인", "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    let grantor = id(&registry, "KB국민카드");
    let read = read_hub(
        &format!("<nav><ul>{menu}</ul></nav><main><ul>{list}</ul></main>"),
        &registry,
        grantor,
        today(),
    );
    assert!(read.listing);
    assert_eq!(read.offers.len(), 6);
    let read = read_hub(&format!("<ul>{menu}</ul>"), &registry, grantor, today());
    assert!(!read.listing, "a value list binding no brand");
}

/// Review M-1: a value under a hidden element refuses the row.
#[test]
fn a_hidden_value_refuses_the_row() {
    let registry = fixture_registry();
    let mut rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5% 할인", "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    rows[0] = kb_row(
        "쿠팡",
        "<span><span class=\"blind\">V</span>50% 할인</span><span style=\"display:none\"><span class=\"blind\">G</span>30% 할인</span>",
        "2026.11.01 ~ 2026.11.30",
        "",
    );
    let read = kb_page(&rows);
    assert_eq!(rejected(&read, &registry, "쿠팡"), ["hidden_value"]);
}

/// Review M-5: "까지", "최고" and "~" bound the value too.
#[test]
fn every_upper_bound_wording_is_an_upper_bound() {
    let registry = fixture_registry();
    let subjects = [
        "50%까지 할인",
        "최고 20% 할인",
        "~30% 할인",
        "최대 10% 할인",
        "10% 할인",
    ];
    let rows: Vec<String> = FIVE
        .iter()
        .zip(subjects)
        .map(|(s, subject)| kb_row(s, subject, "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    let read = kb_page(&rows);
    let bound = |name: &str| offer(&read, &registry, name).map(|o| o.upper_bound);
    assert_eq!(
        FIVE.map(bound),
        [Some(true), Some(true), Some(true), Some(true), Some(false)]
    );
}

/// Review M-6: a bare "할인" badge beside "적립"/"사용" badges does not make
/// a bare amount a discount; a lone 유형 cell "할인" does.
#[test]
fn a_badge_row_is_not_a_discount_cell() {
    let registry = fixture_registry();
    let badges = "<span><i>할인</i><i>적립</i><i>사용</i></span>";
    let rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5,000원", "2026.11.01 ~ 2026.11.30", badges))
        .collect();
    let read = kb_page(&rows);
    assert!(read.offers.is_empty());
    assert_eq!(rejected(&read, &registry, "쿠팡"), ["no_discount_cue"]);
    let rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5,000원", "2026.11.01 ~ 2026.11.30", "<span>할인</span>"))
        .collect();
    assert_eq!(kb_page(&rows).offers.len(), 5);
}

/// Review H-2: a restriction in a sibling cell is shown with the value;
/// a row too long to show is refused.
#[test]
fn a_sibling_restriction_stays_with_the_value() {
    let registry = fixture_registry();
    let mut rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5% 할인", "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    rows[0] = kb_row(
        "쿠팡",
        "10% 할인",
        "2026.11.01 ~ 2026.11.30",
        "<span class=\"who\">신규 가입 고객</span><a href=\"/d\">자세히</a>",
    );
    rows[1] = kb_row(
        "옥션",
        "10% 할인",
        "2026.11.01 ~ 2026.11.30",
        &format!("<span class=\"who\">{}</span>", "대상 상품 ".repeat(20)),
    );
    let read = kb_page(&rows);
    assert_eq!(
        offer(&read, &registry, "쿠팡").map(|o| o.title("KB국민카드")),
        Some("KB국민카드 · 쿠팡 10% 할인 신규 가입 고객".to_string())
    );
    assert_eq!(rejected(&read, &registry, "옥션"), ["statement_too_long"]);
}

/// Security review M2: a page nested deeper than the bound is not read.
#[test]
fn a_page_nested_too_deep_is_not_a_listing() {
    let rows: Vec<String> = FIVE
        .iter()
        .map(|s| kb_row(s, "5% 할인", "2026.11.01 ~ 2026.11.30", ""))
        .collect();
    let deep = format!(
        "{}<ul>{}</ul>{}",
        "<div>".repeat(super::rows::MAX_DEPTH + 10),
        rows.concat(),
        "</div>".repeat(super::rows::MAX_DEPTH + 10)
    );
    let registry = fixture_registry();
    let read = read_hub(&deep, &registry, id(&registry, "KB국민카드"), today());
    assert!(!read.listing && read.offers.is_empty());
    assert_eq!(kb_page(&rows).offers.len(), 5);
}

/// Review H-4: list links are article rows (dated, the link is the row's
/// text), never navigation or footers.
#[test]
fn list_links_are_dated_article_rows_only() {
    let page = reqwest::Url::parse("https://news.example.com/news").unwrap();
    let footer: String = (0..8)
        .map(|i| format!("<li><a href=\"/footer/p{i}\">약관 {i}</a></li>"))
        .collect();
    let menu: String = (0..10)
        .map(|i| {
            format!("<li><a href=\"/card/{i}\">카드 상품 {i}</a> <span>연회비 1만원</span></li>")
        })
        .collect();
    let articles: String = (1..=6)
        .map(|i| format!("<li><a href=\"/news/{i}\">11월 회원 할인 이벤트 안내 {i}</a><span>2026.11.0{i}</span></li>"))
        .collect();
    let links = super::rows::row_links(
        &format!("<ul>{menu}</ul><ul>{articles}</ul><footer><ul>{footer}</ul></footer>"),
        &page,
        40,
    );
    let paths: Vec<&str> = links.iter().map(|u| u.path()).collect();
    assert_eq!(
        paths,
        ["/news/1", "/news/2", "/news/3", "/news/4", "/news/5", "/news/6"]
    );
    // Footers and menus alone: nothing.
    assert!(
        super::rows::row_links(&format!("<ul>{menu}</ul><ul>{footer}</ul>"), &page, 40).is_empty()
    );
    // A dated row whose link is a small "more" next to a long text: not an article row.
    let thin: String = (1..=6)
        .map(|i| format!("<li><span>2026.11.0{i} 매장 리뉴얼 오픈 기념 전 품목 할인 행사 안내</span><a href=\"/n/{i}\">더보기</a></li>"))
        .collect();
    assert!(super::rows::row_links(&format!("<ul>{thin}</ul>"), &page, 40).is_empty());
}

/// Verify finding F6: a list's links are same-host `href`s only: never a
/// script anchor, a composed `data-id`, an off-host or protocol-relative
/// link.
#[test]
fn list_links_never_leave_the_host_or_follow_a_script() {
    let page = reqwest::Url::parse("https://news.example.com/news").unwrap();
    let rows = |href: &dyn Fn(usize) -> String| -> String {
        (1..=6)
            .map(|i| format!("<li><a href=\"{}\" data-id=\"{i}\">11월 할인 이벤트 안내 {i}</a><span>2026.11.0{i}</span></li>", href(i)))
            .collect()
    };
    for bad in [
        &(|_| "javascript:;".to_string()) as &dyn Fn(usize) -> String,
        &|i| format!("https://other.example/news/{i}"),
        &|i| format!("//other.example/news/{i}"),
        &|_| "#".to_string(),
    ] {
        let html = format!("<ul>{}</ul>", rows(bad));
        assert!(
            super::rows::row_links(&html, &page, 40).is_empty(),
            "{html}"
        );
    }
    let good = format!("<ul>{}</ul>", rows(&|i| format!("/news/{i}")));
    assert_eq!(super::rows::row_links(&good, &page, 40).len(), 6);
}
