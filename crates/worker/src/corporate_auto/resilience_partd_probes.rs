use super::*;
use crate::corporate_auto::evidence::visible_text;

const Q: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";

fn shell(body: &str) -> String {
    format!(
        "<html><head><title>t</title></head><body><nav class=\"gnb\"><a href=\"/\">홈</a><a href=\"/student\">학생 혜택</a></nav>\
         <div class=\"container\">{body}</div><footer>고객센터 1588-0000</footer></body></html>"
    )
}
fn adopted(html: &str, quote: &str) -> Resolution {
    resolve(
        &PageView::new(html, &visible_text(html)),
        &StoredFact { quote, baseline: None, ages: Ages { min: None, max: None }, adoption_open: true, anchors: &Anchors::default() },
    )
}
fn sec() -> String { format!("<section><h2>학생 할인</h2><p>{Q}</p></section>") }

#[test]
fn probe_clauses() {
    for c in [
        "학생 할인 서비스 종료 안내",
        "대학생 요금제 서비스 종료되었습니다",
        "본 서비스는 종료되었습니다.",
        "학생 멤버십 서비스 종료",
        "대학생 멤버십이 종료되었습니다.",
        "학생 멤버십 혜택이 만료되었습니다.",
        "선착순 쿠폰 발급이 완료되었습니다.",
        "혜택 제공이 완료되었습니다.",
        "쿠폰이 모두 발급 완료되었습니다.",
        "할인 쿠폰 발급 완료",
        "이벤트 종 료",
        "마 감",
        "종 료",
        "모집 마감 안내",
        "학생 할인 이벤트 조기 마감 안내",
        "이벤트 종료 안내",
        "접수 마감 시간이 지났습니다.",
        "Q: 학생 할인은 아직 되나요 A: 학생 할인 이벤트는 종료되었습니다",
        "If you're looking for the student discount, it has ended",
        "Once again, this offer has ended",
        "When: September 1 to 30, 2026 (this promotion has ended)",
        "The Student Developer Pack is no longer available",
        "Education plan is no longer available",
        "Spotify Premium Student is no longer available in your country",
        "Student plan unavailable",
        "This offer is no longer available",
        "신규 가입이 불가하며 기존 가입자의 경우 혜택이 유지됩니다",
        "학생 요금제는 신규 가입이 불가합니다",
        "학생 요금제 신규 가입 중단",
        "학생 요금제 판매 중단",
        "본 이벤트는 조기 종료되었습니다",
        "자격이 만료되었습니다",
        "학생 회원 자격이 종료되었습니다",
        "학생 할인 구매가 중지됩니다",
        "할인 이용 종료",
        "이용 종료된 혜택입니다",
        "학생 인증 혜택 지원이 종료되었습니다. 홈페이지에서 다른 혜택을 확인하세요",
        "학생 할인 지원 종료 (홈페이지 공지 참고)",
    ] {
        println!("CLAUSE {c:?} => {:?}", text_ending(&squash(c)));
    }
}

#[test]
fn probe_pages() {
    let s = sec();
    let cases: Vec<(&str, String)> = vec![
        ("P0_control_hash_link", format!("<div><a href=\"#none\"><div>종료된 이벤트입니다</div></a></div>{s}")),
        ("P0b_control_plain", format!("<div class=\"end-banner\"><p>본 이벤트는 종료되었습니다. 진행 중인 이벤트 보러가기</p></div>{s}")),
        ("P1_banner_wrapped_in_link", format!("<a href=\"/event/list\" class=\"end-banner\"><div>본 이벤트는 종료되었습니다. 진행 중인 이벤트 보러가기</div></a>{s}")),
        ("P1b_notice_link", format!("<div class=\"notice\"><a href=\"/events\"><p>종료된 이벤트입니다</p></a></div>{s}")),
        ("P2_period_li_with_link", format!("{s}<section><h2>이벤트 안내</h2><ul><li>이벤트 기간: 2026.09.01 ~ 2026.09.30 <a href=\"/notice/1\">자세히 보기</a></li></ul></section>")),
        ("P2b_period_li_plain", format!("{s}<section><h2>이벤트 안내</h2><ul><li>이벤트 기간: 2026.09.01 ~ 2026.09.30</li></ul></section>")),
        ("P3_ending_li_with_link", format!("{s}<ul class=\"notice\"><li>본 이벤트는 조기 종료되었습니다. <a href=\"/events\">다른 이벤트 보기</a></li></ul>")),
        ("P4_slide_li", format!("<ul class=\"slides\"><li><h2>학생 할인</h2><p>{Q}</p><p>이벤트 기간: 2026.09.01 ~ 2026.09.30</p><a href=\"/apply\">신청하기</a></li></ul>")),
        ("P4b_sibling_li", format!("<ul class=\"event\"><li><h2>학생 할인</h2><p>{Q}</p></li><li>본 이벤트는 종료되었습니다 <a href=\"/list\">목록</a></li></ul>")),
        ("P4c_whole_main_in_li", format!("<ul id=\"content\"><li><div class=\"top\"><p>본 이벤트는 종료되었습니다.</p></div>{s}<div><a href=\"/list\">목록</a></div></li></ul>")),
        ("P5_service_end_notice", format!("{s}<section><h2>공지</h2><p>학생 할인 서비스 종료 안내</p></section>")),
        ("P5b_service_ended", format!("{s}<section><h2>공지</h2><p>대학생 요금제 서비스 종료되었습니다</p></section>")),
        ("P6_coupon_issued", format!("{s}<section><h2>공지</h2><p>선착순 쿠폰 발급이 완료되었습니다.</p></section>")),
        ("P7_spaced_badge", format!("<div class=\"status\"><span class=\"badge\">마 감</span></div>{s}")),
        ("P7b_spaced_badge", format!("<div class=\"status\">종 료</div>{s}")),
        ("P8_en_pack", format!("{s}<section><h2>Notice</h2><p>The Student Developer Pack is no longer available.</p></section>")),
        ("P8b_en_plan", format!("{s}<section><h2>Notice</h2><p>Education plan is no longer available.</p></section>")),
        ("P8c_en_if", format!("{s}<section><h2>Notice</h2><p>If you're looking for the student discount, it has ended.</p></section>")),
        ("P9_refusal_cond", format!("{s}<section><h2>공지</h2><p>신규 가입이 불가하며 기존 가입자의 경우 혜택이 유지됩니다</p></section>")),
        ("P10_membership_ended", format!("{s}<section><h2>공지</h2><p>대학생 멤버십이 종료되었습니다.</p></section>")),
        ("P11_qa_oneline", format!("{s}<section><h2>FAQ</h2><p>Q: 학생 할인은 아직 되나요 A: 학생 할인 이벤트는 종료되었습니다</p></section>")),
        ("P12_early_close_heading", format!("<h3>학생 할인 이벤트 조기 마감 안내</h3>{s}")),
        ("P13_end_notice_heading", format!("<h3>이벤트 종료 안내</h3>{s}")),
        ("P14_service_end_in_section", format!("<section><h2>학생 할인</h2><p>{Q}</p><p class=\"notice\">학생 할인 서비스 종료 안내</p></section>")),
        ("P15_coupon_issued_in_section", format!("<section><h2>학생 할인</h2><p>{Q}</p><p>선착순 쿠폰 발급이 완료되었습니다.</p></section>")),
        ("P16_spaced_in_section", format!("<section><h2>학생 할인 <span class=\"badge\">마 감</span></h2><p>{Q}</p></section>")),
        ("N_a_two_signal_with_next_phase", format!("<section><h2>학생 할인</h2><p>{Q}</p><dl><dt>이벤트 기간</dt><dd>2026.09.01 ~ 2026.09.30</dd></dl><p>본 이벤트는 종료되었습니다.</p><p>2차 이벤트는 11월 진행 예정입니다.</p></section>")),
        ("N_b_two_signal_with_other_ongoing_link", format!("<section><h2>학생 할인</h2><p>{Q}</p><dl><dt>이벤트 기간</dt><dd>2026.09.01 ~ 2026.09.30</dd></dl><p>본 이벤트는 종료되었습니다. 진행 중인 다른 이벤트를 확인하세요.</p></section>")),
        ("N_c_two_signal_plain", format!("<section><h2>학생 할인</h2><p>{Q}</p><dl><dt>이벤트 기간</dt><dd>2026.09.01 ~ 2026.09.30</dd></dl><p>본 이벤트는 종료되었습니다.</p></section>")),
        ("N_d_structured_only", format!("<script type=\"application/ld+json\">{{\"@type\":\"Offer\",\"validThrough\":\"2026-09-30\"}}</script>{s}")),
        // hidden text as evidence
        ("H1_sronly_after_toggle", format!("<section><h2>학생</h2><div><button aria-expanded=\"false\">혜택</button></div><p class=\"sr-only\">{Q}</p></section>")),
        ("H2_blind_after_toggle", format!("<section><h2>학생</h2><a class=\"btn-toggle\" href=\"#\">메뉴</a><span class=\"blind\">{Q}</span></section>")),
        ("H3_collapse_dnone", format!("<section><h2>학생</h2><div class=\"collapse d-none\"><p>{Q}</p></div></section>")),
        ("H4_hidden_popup_after_header", format!("<header><button class=\"navbar-toggler\" aria-expanded=\"false\">메뉴</button></header><div style=\"display:none\" class=\"layer-popup\"><p>{Q}</p></div>")),
        ("H5_archive_panel", format!("<section><h2>학생 할인</h2><p>학생 인증 후 5% 할인 혜택을 제공해 드립니다.</p><button aria-controls=\"old\" aria-expanded=\"false\">지난 혜택 보기</button><div id=\"old\" hidden><p>{Q}</p></div></section>")),
        ("H6_collapse_hidden_attr", format!("<section><h2>학생</h2><div class=\"collapse\" hidden><p>{Q}</p></div></section>")),
        ("H7_screen_out_after_toggle_div", format!("<section><h2>학생</h2><div class=\"toggle-wrap\"></div><div class=\"screen_out\">{Q}</div></section>")),
        ("H8_aria_hidden_after_accordion", format!("<section><h2>학생</h2><div class=\"accordion-item\"><span>x</span></div><div aria-hidden=\"true\">{Q}</div></section>")),
        ("H9_old_stale_dnone", format!("<section><h2>학생 할인</h2><button class=\"toggle\">더보기</button><div class=\"d-none old-version\"><p>{Q}</p></div><p>학생 인증 후 5% 할인 혜택을 제공해 드립니다.</p></section>")),
    ];
    for (name, body) in cases {
        let html = shell(&body);
        let view = PageView::new(&html, &visible_text(&html));
        let r = adopted(&html, Q);
        println!("PAGE {name} confirms={} res={:?} page_blocked={:?} quote_in_visible={}", r.confirms(), r, page_blocked(&view, Q), visible_text(&html).contains(Q));
    }
    // digest still covers collapsed panel text
    let a = shell(&format!("<section><h2>학생</h2><button aria-expanded=\"false\">더보기</button><div class=\"collapse\" style=\"display:none\"><p>{Q}</p><p>대상: 대학생</p></div></section>"));
    let b = a.replace("대상: 대학생", "대상: 대학원생");
    let (va, vb) = (PageView::new(&a, &visible_text(&a)), PageView::new(&b, &visible_text(&b)));
    println!("DIGEST panel_change_changes_digest={}", va.page_digest() != vb.page_digest());
}

#[test]
fn probe_ages() {
    use crate::ai_extraction::crosscheck::parse_age;
    for t in [
        "대학생(19~24세)은 10% 할인",
        "만 19세 ~ 24세 대학생 10% 할인",
        "19-24세 10% 할인",
        "13~18세 청소년 및 19~24세 대학생 10% 할인",
        "19세~24세 미만 10% 할인",
        "25세 이상 고객 10% 할인 (19~24세 제외)",
        "19~24세는 할인 대상이 아닙니다. 25세 이상 10% 할인",
        "1~24세 10% 할인",
        "10~20% 할인 (19~24세)",
        "만 19~24세 및 만 65세 이상 10% 할인",
    ] {
        println!("AGE {t:?} parse_age={:?} agree={} starts_b={:?} values_b={:?} sig={:?}", parse_age(t).map(|r| (r.min, r.max)), extractors_agree(t), age_range_starts_b(t), values_b(t), value_signature(t));
    }
}
