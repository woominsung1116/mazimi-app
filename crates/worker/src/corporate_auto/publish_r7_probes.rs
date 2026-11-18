use super::*;
use crate::corporate_auto::bootstrap::extract_offer;
use crate::corporate_auto::evidence::visible_text;

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

fn bare(body: &str) -> String {
    format!("<html><head><title>t</title></head><body><nav><a href=\"/\">홈</a></nav><main>{body}<h2>매장 정보</h2><p>{}</p></main><footer><a href=\"/terms\">이용약관</a></footer></body></html>",
        "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12))
}
#[test]
fn probe_r7_structure() {
    println!("R7 T1b_aside_before => {:?}", live("예시샵", &bare(r##"<aside class="notice"><p>※ 학생 할인은 온라인 스토어 전용입니다.</p></aside><section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section>"##)));
    println!("R7 T1c_aside_after_with_gap => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>오시는 길</h2><p>지하철 2호선 강남역 3번 출구에서 도보 5분 거리입니다.</p><aside class="notice"><p>※ 학생 할인은 온라인 스토어 전용입니다.</p></aside>"##)));
    println!("R7 T3b_hidden_accordion_gap => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>오시는 길</h2><p>지하철 2호선 강남역 3번 출구에서 도보 5분 거리입니다.</p><div><button aria-expanded="false">유의사항</button><div style="display:none"><p>아울렛 상품은 할인되지 않습니다.</p></div></div>"##)));
    println!("R7 T13b_role_button_gap => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>오시는 길</h2><p>지하철 2호선 강남역 3번 출구에서 도보 5분 거리입니다.</p><div role="button" class="notice-box">※ 아울렛 상품은 할인되지 않습니다</div>"##)));
    println!("R7 T15b_nav_gap => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>오시는 길</h2><p>지하철 2호선 강남역 3번 출구에서 도보 5분 거리입니다.</p><nav class="sub"><p>아울렛 상품은 할인되지 않습니다.</p></nav>"##)));
    println!("R7 T16_footer_page_terms => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>오시는 길</h2><p>지하철 2호선 강남역 3번 출구에서 도보 5분 거리입니다.</p>"##)));
    println!("R7 C0_control => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section>"##)));
    println!("R7 T1_aside_callout => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><aside class="notice"><p>※ 학생 할인은 온라인 스토어 전용입니다.</p></aside>"##)));
    println!("R7 T2_article_footer => {:?}", live("예시샵", &bare(r##"<article><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><footer><p>※ 아울렛 매장은 할인되지 않습니다.</p></footer></article>"##)));
    println!("R7 T3_hidden_accordion_outside => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><div><button aria-expanded="false">유의사항</button><div style="display:none"><p>아울렛 상품은 할인되지 않습니다.</p></div></div>"##)));
    println!("R7 T4_inactive_tab => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><div class="tab-pane"><p>아울렛 상품은 할인되지 않습니다.</p></div>"##)));
    println!("R7 T5_image_notice => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><p><img src="/notice.jpg" alt="유의사항: 아울렛 상품 제외"></p>"##)));
    println!("R7 T6_no_listed_word_other_heading => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><h2>사용처</h2><p>직영 매장에서만 사용할 수 있습니다.</p>"##)));
    println!("R7 T7_footnote_id_link => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10%<a href="#fn1">[1]</a> 할인 혜택을 받을 수 있습니다.</p></section><h2>매장</h2><div id="fn1">[1] 직영 매장에서만 사용할 수 있습니다.</div>"##)));
    println!("R7 T8_div_headings => {:?}", live("예시샵", &bare(r##"<div class="title">대학생 할인</div><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><div class="title">유의사항</div><p>아울렛 상품은 할인되지 않습니다.</p>"##)));
    println!("R7 T9_details_outside => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><details><summary>유의사항</summary><p>아울렛 상품은 할인되지 않습니다.</p></details>"##)));
    println!("R7 T10_form_condition => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><form><p>재학생만 인증할 수 있습니다.</p><input name="email"></form>"##)));
    println!("R7 T11_header_hero => {:?}", live("예시샵", &bare(r##"<header class="hero"><p>※ 본 혜택은 신규 회원 전용입니다.</p></header><section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section>"##)));
    println!("R7 T12_column_split => {:?}", live("예시샵", &bare(r##"<div class="row"><div class="col"><section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section></div><div class="col"><p>직영 매장에서만 사용할 수 있습니다.</p></div></div>"##)));
    println!("R7 T13_role_button_notice => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><div role="button" class="notice-box">※ 아울렛 상품은 할인되지 않습니다</div>"##)));
    println!("R7 T14_tooltip_condition => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. <span title="아울렛 제외">ⓘ</span></p></section>"##)));
    println!("R7 T15_nav_inside_main_condition => {:?}", live("예시샵", &bare(r##"<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><nav class="sub"><p>아울렛 상품은 할인되지 않습니다.</p></nav>"##)));
}
