use super::*;
use crate::corporate_auto::bootstrap::{extract_offer, Offer};
use crate::corporate_auto::evidence::visible_text;

fn programme(title: &str, summary: &str, url: &str, m: Option<i32>, o: Option<i32>) -> PublicProgramme {
    PublicProgramme { program_type: "corporate_benefit".into(), title: title.into(), summary: Some(summary.into()), official_url: Some(url.into()),
        application_method: None, eligibility_condition: None, submission_documents: None, screening_method: None, application_steps: None, benefit_conditions: None,
        application_start_at: None, application_end_at: None, deadline_at: None, benefit_valid_until: None,
        benefit_amount_monthly: m, benefit_amount_semester: None, benefit_amount_once: o, min_age: None, max_age: None,
        regions: None, region_districts: None, region_scope: None, school_scope: None, tags: None }
}
fn quotes_of(offer: &Offer) -> Vec<&str> {
    let mut q: Vec<&str> = Vec::new();
    for f in offer.facts.iter().chain(offer.verification.iter()) { if !q.contains(&f.quote.as_str()) { q.push(&f.quote); } }
    q
}
/// What the live publish step would show for this page (title, summary, amounts) and whether it would publish.
fn run(name: &str, brand: &str, html: &str, url: &str) {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let offer = match extract_offer(html, today) { Ok(o) => o, Err(e) => { println!("R5V {name} extract_err={e}"); return; } };
    let quotes = quotes_of(&offer);
    let scan = page_blocked_scan(html, &quotes);
    let (m, o) = grammar_amounts(&quotes);
    let title = korean_title(brand, &offer).unwrap_or_default();
    let summary = verbatim_summary(&summary_quotes(&offer));
    let p = programme(&title, &summary, url, m, o);
    let verdict = verify_verbatim_fields(&p, &offer, &quotes, html, &visible_text(html), url);
    println!("R5V {name} scan={scan:?} verify={verdict:?} title={title:?} summary={summary:?} monthly={m:?} once={o:?}");
}
fn page(body: &str) -> String {
    format!("<html><head><title>t</title></head><body><main><h1>대학생 할인 프로그램</h1>{body}<div class=\"about\"><p>본 프로그램은 대학생과 대학원생을 위한 학생 할인 혜택입니다. 자세한 사항은 매장 직원에게 문의하시기 바랍니다. 학생 할인은 온라인 스토어와 오프라인 매장에서 이용할 수 있으며 학생증으로 재학 여부를 확인합니다.</p><p>매장 운영 시간은 매일 오전 9시부터 오후 10시까지이며 명절 당일에는 일부 매장이 휴무할 수 있습니다. 온라인 주문은 연중무휴로 가능하며 배송은 영업일 기준 2~3일이 소요됩니다. 고객센터는 평일 오전 9시부터 오후 6시까지 운영합니다.</p><p>매장 위치와 운영 정보는 매장 찾기 메뉴에서 확인하실 수 있습니다. 회원 가입은 무료이며 누구나 앱에서 가입할 수 있습니다. 개인정보 처리방침과 이용약관은 하단 링크에서 확인할 수 있습니다.</p></div><p>{}</p></main></body></html>", "매장과 온라인에서 이용할 수 있는 안내 문구입니다. ".repeat(20))
}

#[test]
fn probe_r5_verbatim() {
    let u = "https://shop.example.com/student";
    run("V1_condition_next_block", "예시샵", &page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p>"), u);
    run("V2_long_line_cut", "예시샵", &page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있으며 이 혜택은 매장을 방문하시는 모든 고객님을 위한 특별 프로모션으로 운영되고 있습니다 다만 3만원 이상 구매하신 경우에만 적용되며 일부 매장은 대상에서 빠집니다</p>"), u);
    run("V3_monthly_amount_cap_next", "예시샵", &page("<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>※ 가입 후 3개월 동안만 제공됩니다.</p>"), u);
    run("V4_other_audience_section", "예시샵", &page("<section><h2>대학생</h2><p>대학생 10% 할인</p></section><section><h2>청소년</h2><p>청소년 매월 5,000원 할인 쿠폰을 드립니다</p></section>"), u);
    run("V5_brand_with_digit", "11번가", &page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"), u);
    run("V6_exclusion_next", "예시샵", &page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 아메리카노는 할인 대상에서 제외됩니다.</p>"), u);
    run("V7_member_only_next", "예시샵", &page("<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>※ 유료 멤버십(월 4,900원) 가입 고객에 한합니다.</p>"), u);
    run("V8_period_next_future", "예시샵", &page("<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>이벤트 기간: 2026.10.01 ~ 2026.10.31</p>"), u);
    for (name, q) in [
        ("N1_first_month_then", "대학생 첫 달 5,000원 할인 쿠폰을 드립니다"),
        ("N2_gaip_si", "대학생 가입 시 5,000원 할인 쿠폰을 드립니다"),
        ("N3_per_purchase", "대학생 결제 시 5,000원 할인"),
        ("N4_monthly_two_values", "대학생 매월 5,000원 할인 쿠폰과 매월 3,000원 적립"),
        ("N5_jiwon_monthly", "대학생 매월 10만원 지원"),
        ("N6_teen_amount", "청소년 매월 5,000원 할인 쿠폰을 드립니다"),
        ("N7_youth_vs_student", "대학생 매월 5,000원 할인 쿠폰을 드립니다 청년"),
        ("N8_cheon", "대학생 매월 5천원 할인"),
        ("N9_man_won_space", "대학생 매월 1만 원 할인"),
        ("N10_ilhoe_first", "대학생 1회 5,000원 할인 쿠폰"),
        ("N11_daily_price", "대학생 매일 1,000원 할인"),
        ("N12_won_off_price", "대학생 월 5,000원 할인 요금"),
        ("N13_once_choechul", "대학생 최초 가입 시 5,000원 할인 쿠폰을 드립니다"),
        ("N14_two_wons_one_clause", "대학생 매월 5,000원 할인 쿠폰 또는 3,000원 적립"),
        ("N15_paren_note", "대학생 매월 5,000원 할인 쿠폰을 드립니다(선착순)"),
        ("N16_seonchak", "선착순 대학생 매월 5,000원 할인 쿠폰을 드립니다"),
        ("N17_sale_price", "대학생 전용 가격 매월 9,900원"),
        ("N18_point", "대학생 매월 5,000포인트 적립"),
        ("N19_cashback_once", "대학생 첫 결제 시 3,000원 캐시백"),
        ("N20_yeongan", "대학생 매월 5,000원 할인 연간 최대 6만원"),
    ] {
        println!("R5N {name} => {:?} agree={}", grammar_amounts(&[q]), crate::corporate_auto::resilience::extractors_agree(q));
    }
}

#[test]
fn probe_r5_cost() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real_pages/");
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}index.json")).unwrap()).unwrap();
    let audit = crate::corporate_auto::evidence::audit();
    let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
    for e in index.as_array().unwrap() {
        let Some(file) = e["file"].as_str() else { *tally.entry("no_file".into()).or_default() += 1; continue };
        let Ok(html) = std::fs::read_to_string(format!("{dir}{file}")) else { *tally.entry("unreadable".into()).or_default() += 1; continue };
        let url = e["final_url"].as_str().or(e["url"].as_str()).unwrap_or("");
        let brand = audit.items.iter().find(|i| i.id.to_string() == e["program_id"].as_str().unwrap_or("")).map(|i| i.provider.clone()).unwrap_or_default();
        let outcome = match extract_offer(&html, today) {
            Err(err) => format!("extract:{err}"),
            Ok(offer) => {
                let quotes = quotes_of(&offer);
                match page_blocked_scan(&html, &quotes) {
                    ScanVerdict::Ended(r) => format!("scan_ended:{}", crate::corporate_auto::resilience::reason_key(&r).unwrap_or("?")),
                    ScanVerdict::Clean => {
                        let (m, o) = grammar_amounts(&quotes);
                        let p = programme(&korean_title(&brand, &offer).unwrap_or_default(), &verbatim_summary(&summary_quotes(&offer)), url, m, o);
                        match verify_verbatim_fields(&p, &offer, &quotes, &html, &visible_text(&html), url) {
                            Ok(()) => { println!("R5COST_OK {:?} title={:?} summary={:?} m={m:?} o={o:?}", e["title"], p.title, p.summary); "publish".into() }
                            Err(r) => format!("verify:{r}"),
                        }
                    }
                }
            }
        };
        println!("R5COST {:?} => {outcome}", e["title"].as_str().unwrap_or(""));
        *tally.entry(outcome.split(':').take(2).collect::<Vec<_>>().join(":")).or_default() += 1;
    }
    println!("R5COST_TALLY {tally:?}");
}
