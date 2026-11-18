use super::*;
use crate::corporate_auto::evidence::visible_text;

const Q: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
const NEW: &str = "학생 인증 후 5% 할인 혜택을 제공해 드립니다.";
fn shell(body: &str) -> String {
    format!("<html><head><title>t</title></head><body><nav class=\"gnb\"><a href=\"/\">홈</a><a href=\"/student\">학생 혜택</a></nav><div class=\"container\">{body}</div><footer>고객센터 1588-0000</footer></body></html>")
}
fn adopted(html: &str) -> Resolution {
    resolve(&PageView::new(html, &visible_text(html)), &StoredFact { quote: Q, baseline: None, ages: Ages { min: None, max: None }, adoption_open: true, anchors: &Anchors::default() })
}
fn sec() -> String { format!("<section><h2>학생 할인</h2><p>{Q}</p></section>") }

#[test]
fn probe_r2_clauses() {
    for c in [
        "R1 대학생 멤버십 서비스는 9월 30일 종료되었으며 기존 회원의 경우 적립 포인트는 유지됩니다",
        "R2 학생 요금제는 2026년 9월 30일 기준으로 서비스가 종료되었습니다",
        "R3 시스템 개편 등으로 학생 할인 서비스가 종료되었습니다",
        "R4 학생 할인 서비스는 종료되었으나 기존 쿠폰은 사용 가능합니다",
        "R5 2026년 10월 1일부터 대학생 멤버십 서비스가 종료됩니다",
        "R6 학생 멤버십 종료 안내 기존 회원의 경우 포인트는 유지됩니다",
        "R7 대학생 요금제 가입 시 제공되던 학생 할인 서비스가 종료되었습니다",
        "R8 학생 회원 혜택은 10월부터 종료되었습니다",
        "R9 10월 1일부터 학생 할인 이용 종료",
        "U1 선착순 접수가 완료되었습니다.",
        "U2 접수완료",
        "U3 신청 접수가 모두 완료되었습니다",
        "U4 선착순 100명 참여가 완료되었습니다",
        "U5 응모 접수 완료",
        "F1 학생 인증을 하신 경우에도 현재 쿠폰 발급이 불가합니다",
        "F2 이미 인증하신 경우라도 신규 가입이 불가합니다",
        "F3 대상자인 경우에도 더 이상 신청이 불가능합니다",
        "E1 The education bundle is no longer available",
        "E2 Campus perks are unavailable",
        "E3 Free Pro for academics is no longer available",
        "S1 마  감",
    ] {
        println!("R2CLAUSE {c:?} => {:?}", text_ending(&squash(c)));
    }
}

#[test]
fn probe_r2_pages() {
    let s = sec();
    let p = |name: &str, body: String| {
        let html = shell(&body);
        let view = PageView::new(&html, &visible_text(&html));
        let r = adopted(&html);
        println!("R2PAGE {name} confirms={} res={:?} page_blocked={:?}", r.confirms(), r, page_blocked(&view, Q));
    };
    for (name, line) in [
        ("R1", "대학생 멤버십 서비스는 9월 30일 종료되었으며 기존 회원의 경우 적립 포인트는 유지됩니다"),
        ("R2", "학생 요금제는 2026년 9월 30일 기준으로 서비스가 종료되었습니다"),
        ("R3", "시스템 개편 등으로 학생 할인 서비스가 종료되었습니다"),
        ("R4", "학생 할인 서비스는 종료되었으나 기존 쿠폰은 사용 가능합니다"),
        ("R5", "2026년 10월 1일부터 대학생 멤버십 서비스가 종료됩니다"),
        ("R7", "대학생 요금제 가입 시 제공되던 학생 할인 서비스가 종료되었습니다"),
        ("R8", "학생 회원 혜택은 10월부터 종료되었습니다"),
        ("U1", "선착순 접수가 완료되었습니다."),
        ("U3", "신청 접수가 모두 완료되었습니다"),
        ("F1", "학생 인증을 하신 경우에도 현재 쿠폰 발급이 불가합니다"),
        ("E1", "The education bundle is no longer available"),
    ] {
        p(&format!("{name}_notice"), format!("{s}<section><h2>공지</h2><p>{line}</p></section>"));
        p(&format!("{name}_in_section"), format!("<section><h2>학생 할인</h2><p>{Q}</p><p>{line}</p></section>"));
    }
    p("U2_badge", format!("<div class=\"status\"><span class=\"badge\">접수완료</span></div>{s}"));
    // fix 7: own offer's banner in a carousel grid
    p("G1_carousel_stock", format!("<ul class=\"banners\"><li><a href=\"/e/1\">학생 할인 쿠폰 선착순 재고 소진</a></li><li><a href=\"/e/2\">가을 신상품 이벤트</a></li></ul>{s}"));
    p("G2_coupon_tile_soldout_with_quote", format!("<ul class=\"coupons\"><li><a href=\"/c/1\"><p>{Q}</p><span>품절</span></a></li><li><a href=\"/c/2\"><p>신규 회원 3,000원 쿠폰</p></a></li></ul>"));
    p("G3_student_offer_soldout_en", format!("<ul><li><a href=\"/o/1\">Student offer sold out</a></li><li><a href=\"/o/2\">Holiday deals</a></li></ul>{s}"));
    // fix 8: stale value only in a panel, visible text has a new value
    let panel = |toggle: &str| format!("<section><h2>학생 할인</h2><p>{NEW}</p>{toggle}<div hidden><p>{Q}</p></div></section>");
    p("H10_change_before", panel("<button aria-expanded=\"false\">변경 전 혜택 보기</button>"));
    p("H11_header_past_label", format!("<section><h2>학생 할인</h2><p>{NEW}</p><div class=\"acc-head\"><h3>2025년 지난 혜택</h3><button aria-expanded=\"false\" aria-label=\"펼치기\"></button></div><div hidden><p>{Q}</p></div></section>"));
    p("H12_year_label", panel("<button aria-expanded=\"false\">2025년 혜택 보기</button>"));
    p("H13_legacy_en", panel("<button aria-expanded=\"false\">Legacy plan details</button>"));
    p("H14_far_aria_controls", format!("<section><h2>학생 할인</h2><p>{NEW}</p><p><a href=\"#\" aria-controls=\"t2025\">자세히</a></p></section><section><div id=\"t2025\" style=\"display:none\"><p>{Q}</p></div></section>"));
    p("H15_old_kr", panel("<button aria-expanded=\"false\">구 요금제 혜택</button>"));
}
