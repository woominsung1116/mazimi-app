use super::*;
use crate::corporate_auto::bootstrap::extract_offer;
use crate::corporate_auto::evidence::visible_text;

    fn page(body: &str) -> String {
        format!("<html><head><title>t</title></head><body><main><h1>대학생 할인 프로그램</h1>{body}<div class=\"about\"><p>본 프로그램은 대학생과 대학원생을 위한 학생 할인 혜택입니다. 자세한 사항은 매장 직원에게 문의하시기 바랍니다. 학생 할인은 온라인 스토어와 오프라인 매장에서 이용할 수 있으며 학생증으로 재학 여부를 확인합니다.</p><p>매장 운영 시간은 매일 오전 9시부터 오후 10시까지이며 명절 당일에는 일부 매장이 휴무할 수 있습니다. 온라인 주문은 연중무휴로 가능하며 배송은 영업일 기준 2~3일이 소요됩니다. 고객센터는 평일 오전 9시부터 오후 6시까지 운영합니다.</p><p>매장 위치와 운영 정보는 매장 찾기 메뉴에서 확인하실 수 있습니다. 회원 가입은 무료이며 누구나 앱에서 가입할 수 있습니다. 개인정보 처리방침과 이용약관은 하단 링크에서 확인할 수 있습니다.</p></div><p>{}</p></main></body></html>", "매장과 온라인에서 이용할 수 있는 안내 문구입니다. ".repeat(20))
    }

    fn live(brand: &str, html: &str) -> Result<String, String> {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).expect("date");
        let offer = extract_offer(html, today).map_err(|e| format!("extract:{e}"))?;
        let mut quotes: Vec<&str> = Vec::new();
        for f in offer.facts.iter().chain(offer.verification.iter()) {
            if !quotes.contains(&f.quote.as_str()) {
                quotes.push(&f.quote);
            }
        }
        if !matches!(page_blocked_scan(html, &quotes), ScanVerdict::Clean) {
            return Err("scan".into());
        }
        let display =
            compose_display(html, &offer).map_err(|r| format!("display:{r}"))?;
        let programme = PublicProgramme {
            program_type: "corporate_benefit".into(),
            title: korean_title(brand, &offer).map_err(|r| format!("title:{r}"))?,
            summary: Some(display.clone()),
            official_url: Some("https://shop.example.com/student".into()),
            application_method: None,
            eligibility_condition: None,
            submission_documents: None,
            screening_method: None,
            application_steps: None,
            benefit_conditions: None,
            application_start_at: None,
            application_end_at: None,
            deadline_at: None,
            benefit_valid_until: None,
            benefit_amount_monthly: None,
            benefit_amount_semester: None,
            benefit_amount_once: None,
            min_age: None,
            max_age: None,
            regions: None,
            region_districts: None,
            region_scope: None,
            school_scope: None,
            tags: None,
        };
        verify_verbatim_fields(
            &programme,
            &offer,
            &quotes,
            html,
            &visible_text(html),
            "https://shop.example.com/student",
        )?;
        Ok(display)
    }

#[test]
fn probe_r6_blocks() {
    println!("R6 A1_condition_two_blocks_away => {:?}", live("예시샵", &page(r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>학생증은 실물만 인정됩니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p>"#)));
    println!("R6 A2_table_condition_hanjeong => {:?}", live("예시샵", &page(r#"<table><tr><th>혜택</th><td>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</td></tr><tr><th>적용 매장</th><td>직영 매장 한정</td></tr></table>"#)));
    println!("R6 A3_footnote_sup => {:?}", live("예시샵", &page(r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. <sup>1)</sup></p><p>매장 정보는 아래를 참고해 주세요.</p>"#)));
    println!("R6 A3b_footnote_far => {:?}", live("예시샵", &page(r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>매장 정보는 아래를 참고해 주세요.</p><p>각주: 아울렛 상품은 할인되지 않습니다.</p>"#)));
    println!("R6 A4_sibling_list_no_keyword => {:?}", live("예시샵", &page(r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><ul><li>온라인 스토어 전용 혜택입니다.</li><li>1인 1회 사용할 수 있습니다.</li></ul>"#)));
    println!("R6 A5_period_without_cue => {:?}", live("예시샵", &page(r#"<section><h2>학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><section><h2>안내</h2><p>본 할인은 2026년 11월 30일까지 진행합니다.</p></section>"#)));
    println!("R6 A6_note_before => {:?}", live("예시샵", &page(r#"<p>※ 아래 혜택은 대학원생에게만 적용됩니다.</p><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#)));
    println!("R6 A7_heading_condition => {:?}", live("예시샵", &page(r#"<h2>학생 할인 (온라인 전용)</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#)));
    println!("R6 A8_exclusion_not_keyword => {:?}", live("예시샵", &page(r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>참고로 아울렛 상품은 할인되지 않습니다.</p>"#)));
    println!("R6 A9_english_not_valid => {:?}", live("예시샵", &page(r#"<p>Verified university students get 10% off all full-price items.</p><p>Not valid on sale items or gift cards.</p>"#)));
    println!("R6 A10_mobile_copy_first => {:?}", live("예시샵", &page(r#"<div class="mobile"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></div><div class="pc"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. 단, 아울렛 매장은 빠집니다.</p></div>"#)));
    println!("R6 A11_note_in_next_container => {:?}", live("예시샵", &page(r#"<div class="offer"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></div><div class="note"><p>참고로 아울렛 상품은 할인되지 않습니다.</p></div>"#)));
    println!("R6 A12_member_required_prev => {:?}", live("예시샵", &page(r#"<p>나이키 멤버 전용 혜택</p><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#)));
}
