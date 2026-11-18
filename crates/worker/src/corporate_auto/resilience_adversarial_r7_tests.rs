//! Round-7 independent adversarial replay (reviewer-written) against the
//! whole-page contract (handoff §16/§16b). Same harness as r6: seed (adopted
//! exact confirm) -> confirm again with captured anchors -> resolve the
//! after-page with the live baseline and anchors. Mounted under
//! `resilience::tests`, so it may set the private `PageView::today` to
//! replay an unchanged page while time passes. Public API otherwise.

use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::resilience::{capture, resolve, Anchors, PageView, Resolution, StoredFact};
use chrono::NaiveDate;

const NO_AGES: Ages = Ages { min: None, max: None };
const STUDENT: &str = "학생 인증 시 10% 할인";

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
fn today() -> NaiveDate { d(2026, 10, 18) }

fn page3(top: &str, main: &str, bottom: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>혜택 안내</title></head>\
         <body>{top}<header class=\"gnb\"><a href=\"/\">홈</a> <a href=\"/benefit\">혜택</a></header>\
         <main id=\"content\">{main}</main>{bottom}<footer class=\"ft\"><p>고객센터 1588-1234</p></footer></body></html>"
    )
}
fn page(main: &str) -> String { page3("", main, "") }
fn view_at(html: &str, at: NaiveDate) -> PageView { let mut v = PageView::new(html, &visible_text(html)); v.today = at; v }

struct Stored { quote: String, baseline: String, anchors: Anchors }

fn fact<'a>(q: &'a str, b: Option<&'a str>, a: &'a Anchors, open: bool) -> StoredFact<'a> {
    StoredFact { quote: q, baseline: b, ages: NO_AGES, adoption_open: open, anchors: a }
}

fn seed(html: &str, quote: &str, at: NaiveDate) -> Result<Stored, String> {
    let v = view_at(html, at);
    let empty = Anchors::default();
    let window = match resolve(&v, &fact(quote, None, &empty, true)) {
        Resolution::Exact(FactOutcome::Confirmed { window_signature, adopted: true }) => window_signature,
        other => return Err(format!("seed not adopted: {other:?}")),
    };
    let anchors = Anchors { fingerprint: capture(&v, quote), ..Anchors::default() };
    if anchors.fingerprint.is_none() { return Err("capture found no holder".into()); }
    let second = resolve(&v, &fact(quote, Some(&window), &anchors, false));
    if !matches!(second, Resolution::Exact(FactOutcome::Confirmed { .. })) {
        return Err(format!("second confirm failed: {second:?}"));
    }
    let anchors = Anchors { fingerprint: capture(&v, quote), ..anchors };
    Ok(Stored { quote: quote.into(), baseline: window, anchors })
}

fn after(s: &Stored, html: &str, at: NaiveDate) -> Resolution {
    resolve(&view_at(html, at), &fact(&s.quote, Some(&s.baseline), &s.anchors, false))
}

#[derive(Clone, Copy, Debug)]
enum Expect { Same, Gone }

#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
enum Verdict { Confirmed, Hidden, FalseHide, FalseConfirm, WrongUpdate, SeedFailed }

fn judge(e: Expect, r: &Resolution) -> Verdict {
    let proposed = matches!(r, Resolution::Proposed(_));
    match e {
        _ if proposed => Verdict::WrongUpdate,
        Expect::Same if r.confirms() => Verdict::Confirmed,
        Expect::Same => Verdict::FalseHide,
        Expect::Gone if r.confirms() => Verdict::FalseConfirm,
        Expect::Gone => Verdict::Hidden,
    }
}

/// Changed page (same `today`), or an unchanged page replayed later.
struct Case { id: &'static str, name: &'static str, quote: &'static str, before: String, seed_at: NaiveDate, after: String, after_at: NaiveDate, expect: Expect }
fn chg(id: &'static str, name: &'static str, quote: &'static str, before: String, after: String, expect: Expect) -> Case {
    Case { id, name, quote, before, seed_at: today(), after, after_at: today(), expect }
}
fn later(id: &'static str, name: &'static str, html: String, seed_at: NaiveDate, after_at: NaiveDate, expect: Expect) -> Case {
    Case { id, name, quote: STUDENT, before: html.clone(), seed_at, after: html, after_at, expect }
}

/// First sight at `today()`: is a fact adopted from this page?
struct First { id: &'static str, name: &'static str, quote: &'static str, html: String, expect: Expect }
fn first(id: &'static str, name: &'static str, html: String, expect: Expect) -> First { First { id, name, quote: STUDENT, html, expect } }

fn sec(offer: &str) -> String {
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{offer}</p><p class=\"desc\">인증은 매 학기 갱신됩니다.</p></section>")
}
fn sec_note(note: &str) -> String {
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><p class=\"note\">{note}</p></section>")
}
fn sec_extra(extra: &str) -> String {
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p>{extra}</section>")
}
fn ld(json: &str) -> String { format!("<script type=\"application/ld+json\">{json}</script>") }

fn changed_cases() -> Vec<Case> {
    vec![
        // ── (a) meaning changes that survive the digest fold ──
        chg("A1", "board-counter fold: condition '댓글 3개 이상' -> '댓글 10개 이상'", STUDENT,
            page(&sec_note("댓글 3개 이상 작성 시 응모 완료")), page(&sec_note("댓글 10개 이상 작성 시 응모 완료")), Expect::Gone),
        chg("A2", "board-counter fold: '친구 추천 3명 달성 시' -> '친구 추천 5명'", STUDENT,
            page(&sec_note("친구 추천 3명 달성 시 추가 5,000원 적립")), page(&sec_note("친구 추천 5명 달성 시 추가 5,000원 적립")), Expect::Gone),
        chg("A3", "board-counter fold: '좋아요 100개 달성 시' -> '좋아요 1000개'", STUDENT,
            page(&sec_note("좋아요 100개 달성 시 쿠폰 지급")), page(&sec_note("좋아요 1000개 달성 시 쿠폰 지급")), Expect::Gone),
        chg("A4", "D±N fold: '가입 후 D+30일 이내' -> 'D+7일 이내'", STUDENT,
            page(&sec_note("가입 후 D+30일 이내 첫 구매 시 적용")), page(&sec_note("가입 후 D+7일 이내 첫 구매 시 적용")), Expect::Gone),
        chg("A5", "D±N fold: countdown 'D-3' -> 'D+3' (deadline passed 3 days ago)", STUDENT,
            page(&sec_extra("<p class=\"dday\">응모 D-3</p>")), page(&sec_extra("<p class=\"dday\">응모 D+3</p>")), Expect::Gone),
        chg("A6", "relative-time fold: '이용 2시간 전까지 예약 시' -> '24시간 전까지'", STUDENT,
            page(&sec_note("이용 2시간 전까지 예약 시 적용")), page(&sec_note("이용 24시간 전까지 예약 시 적용")), Expect::Gone),
        chg("A7", "labelled-date fold: eligibility '회원 등록일 2026.01.01 이후 가입' -> '2026.06.01 이후'", STUDENT,
            page(&sec_note("회원 등록일 2026.01.01 이후 가입 고객 대상")), page(&sec_note("회원 등록일 2026.06.01 이후 가입 고객 대상")), Expect::Gone),
        chg("A8", "markup: rowspan removed, 대학원생 no longer under the 10% cell (text identical)", STUDENT,
            page(&sec_extra("<table class=\"rate\"><tr><th>대학생</th><td rowspan=\"2\">10% 할인</td></tr><tr><th>대학원생</th></tr></table>")),
            page(&sec_extra("<table class=\"rate\"><tr><th>대학생</th><td>10% 할인</td></tr><tr><th>대학원생</th></tr></table>")), Expect::Gone),
        chg("A9", "markup: 대학원생 moved from the 10% column to the 5% column (text order identical)", STUDENT,
            page(&sec_extra("<table class=\"grid\"><tr><th>10% 할인</th><th>5% 할인</th></tr><tr><td><p>대학생</p><p>대학원생</p></td><td><p>교직원</p></td></tr></table>")),
            page(&sec_extra("<table class=\"grid\"><tr><th>10% 할인</th><th>5% 할인</th></tr><tr><td><p>대학생</p></td><td><p>대학원생</p><p>교직원</p></td></tr></table>")), Expect::Gone),
        chg("A10", "visible button text in <input value>: '응모하기' -> '응모 마감' + disabled", STUDENT,
            page(&sec_extra("<form action=\"/apply\"><input type=\"submit\" class=\"btn\" value=\"응모하기\"></form>")),
            page(&sec_extra("<form action=\"/apply\"><input type=\"submit\" class=\"btn\" value=\"응모 마감\" disabled></form>")), Expect::Gone),
        chg("A11", "inline <svg><text> badge '진행중' -> '종료'", STUDENT,
            page(&sec_extra("<svg class=\"badge\" width=\"60\" height=\"20\"><text x=\"0\" y=\"15\">진행중</text></svg>")),
            page(&sec_extra("<svg class=\"badge\" width=\"60\" height=\"20\"><text x=\"0\" y=\"15\">종료</text></svg>")), Expect::Gone),
        chg("A12", "comma fold merges numbers: '쿠폰 1,2차 지급' -> '쿠폰 12차 지급' (low realism)", STUDENT,
            page(&sec_note("쿠폰 1,2차 지급")), page(&sec_note("쿠폰 12차 지급")), Expect::Gone),
        chg("A13", "control: '댓글 3개' -> '댓글 3개 이상' (word added)", STUDENT,
            page(&sec_note("댓글 3개 작성 시 응모 완료")), page(&sec_note("댓글 3개 이상 작성 시 응모 완료")), Expect::Gone),
        // ── (f) JSON-LD: visible text unchanged ──
        chg("F1", "JSON-LD availability InStock -> SoldOut", STUDENT,
            page(&format!("{}{}", ld("{\"@type\":\"Product\",\"offers\":{\"@type\":\"Offer\",\"availability\":\"https://schema.org/InStock\",\"validThrough\":\"2026-12-31\"}}"), sec(STUDENT))),
            page(&format!("{}{}", ld("{\"@type\":\"Product\",\"offers\":{\"@type\":\"Offer\",\"availability\":\"https://schema.org/SoldOut\",\"validThrough\":\"2026-12-31\"}}"), sec(STUDENT))), Expect::Gone),
        chg("F2", "JSON-LD eventStatus EventScheduled -> EventCancelled", STUDENT,
            page(&format!("{}{}", ld("{\"@type\":\"Event\",\"startDate\":\"2026-10-01\",\"endDate\":\"2026-12-31\",\"eventStatus\":\"https://schema.org/EventScheduled\"}"), sec(STUDENT))),
            page(&format!("{}{}", ld("{\"@type\":\"Event\",\"startDate\":\"2026-10-01\",\"endDate\":\"2026-12-31\",\"eventStatus\":\"https://schema.org/EventCancelled\"}"), sec(STUDENT))), Expect::Gone),
        chg("F3", "microdata <meta itemprop=validThrough> 2026-12-31 -> 2026-10-13", STUDENT,
            page(&format!("<div itemscope itemtype=\"https://schema.org/Offer\"><meta itemprop=\"validThrough\" content=\"2026-12-31\">{}</div>", sec(STUDENT))),
            page(&format!("<div itemscope itemtype=\"https://schema.org/Offer\"><meta itemprop=\"validThrough\" content=\"2026-10-13\">{}</div>", sec(STUDENT))), Expect::Gone),
        chg("F4", "JSON-LD type 'application/ld+json; charset=utf-8', validThrough -> 2026-10-13", STUDENT,
            page(&format!("<script type=\"application/ld+json; charset=utf-8\">{{\"@type\":\"Offer\",\"validThrough\":\"2026-12-31\"}}</script>{}", sec(STUDENT))),
            page(&format!("<script type=\"application/ld+json; charset=utf-8\">{{\"@type\":\"Offer\",\"validThrough\":\"2026-10-13\"}}</script>{}", sec(STUDENT))), Expect::Gone),
        chg("F5", "JSON-LD compact IRI key 'schema:validThrough' -> 2026-10-13", STUDENT,
            page(&format!("{}{}", ld("{\"@context\":{\"schema\":\"https://schema.org/\"},\"@type\":\"schema:Offer\",\"schema:validThrough\":\"2026-12-31\"}"), sec(STUDENT))),
            page(&format!("{}{}", ld("{\"@context\":{\"schema\":\"https://schema.org/\"},\"@type\":\"schema:Offer\",\"schema:validThrough\":\"2026-10-13\"}"), sec(STUDENT))), Expect::Gone),
        chg("F6", "SSR markup unchanged, hydration data status OPEN -> ENDED (client-rendered state; limit)", STUDENT,
            page(&format!("{}<script id=\"__NEXT_DATA__\" type=\"application/json\">{{\"props\":{{\"event\":{{\"status\":\"OPEN\"}}}}}}</script>", sec(STUDENT))),
            page(&format!("{}<script id=\"__NEXT_DATA__\" type=\"application/json\">{{\"props\":{{\"event\":{{\"status\":\"ENDED\"}}}}}}</script>", sec(STUDENT))), Expect::Gone),
        // ── (d) unchanged page, the world changed ──
        later("D1", "yearless '~ 3월 31일까지' seen 2026-03-01, replayed 2026-10-18 (read as 2027)",
              page(&sec_note("이벤트 기간 ~ 3월 31일까지")), d(2026, 3, 1), today(), Expect::Gone),
        later("D2", "'이번 달 말까지' seen 2026-09-10, replayed 2026-10-18",
              page(&sec_note("이번 달 말까지 진행")), d(2026, 9, 10), today(), Expect::Gone),
        later("D3", "'오늘까지' seen 2026-10-10, replayed 2026-10-18",
              page(&sec_note("오늘까지만 진행되는 혜택")), d(2026, 10, 10), today(), Expect::Gone),
        later("D4", "yearless month-end '9월 말까지' seen 2026-09-01",
              page(&sec_note("9월 말까지 진행")), d(2026, 9, 1), today(), Expect::Gone),
        later("D5", "yearless month '9월까지 진행' seen 2026-09-01",
              page(&sec_note("신학기 맞이 9월까지 진행")), d(2026, 9, 1), today(), Expect::Gone),
        later("D6", "'9월 한 달간 진행' seen 2026-09-01",
              page(&sec_note("9월 한 달간 진행")), d(2026, 9, 1), today(), Expect::Gone),
        later("D7", "'2026년 9월 한정' (month with year, no 까지) seen 2026-09-01",
              page(&sec_note("2026년 9월 한정 혜택")), d(2026, 9, 1), today(), Expect::Gone),
        later("D8", "'10월 13일 오후 6시까지' seen 2026-10-01",
              page(&sec_note("2026년 10월 13일 오후 6시까지")), d(2026, 10, 1), today(), Expect::Gone),
        later("D9", "'2026년 10월 13일에 종료됩니다' seen 2026-10-01",
              page(&sec_note("본 이벤트는 2026년 10월 13일에 종료됩니다")), d(2026, 10, 1), today(), Expect::Gone),
        later("D10", "'마감일은 2026년 10월 13일입니다' seen 2026-10-01",
              page(&sec_note("신청 마감일은 2026년 10월 13일입니다")), d(2026, 10, 1), today(), Expect::Gone),
        later("D11", "'2026년 말까지' seen 2026-06-01, replayed 2027-01-15",
              page(&sec_note("2026년 말까지 진행")), d(2026, 6, 1), d(2027, 1, 15), Expect::Gone),
        later("D12", "'10월 13일부로 종료' (no verb) seen 2026-10-01",
              page(&sec_note("본 이벤트는 2026년 10월 13일부로 종료")), d(2026, 10, 1), today(), Expect::Gone),
        later("D13", "control '~ 2026.10.13까지' seen 2026-10-01",
              page(&sec_note("이벤트 기간 ~ 2026.10.13까지")), d(2026, 10, 1), today(), Expect::Gone),
        later("D14", "control yearless '~ 3월 31일까지' seen 2026-03-01, replayed 2026-05-01",
              page(&sec_note("이벤트 기간 ~ 3월 31일까지")), d(2026, 3, 1), d(2026, 5, 1), Expect::Gone),
        later("D15", "end day, '2026년 10월 18일 오전 10시까지', replayed 10-18 (wrong after 10:00 KST)",
              page(&sec_note("2026년 10월 18일 오전 10시까지")), d(2026, 10, 1), today(), Expect::Gone),
    ]
}

fn first_cases() -> Vec<First> {
    let top = |t: &str| page3(&format!("<div class=\"top-notice\"><p>{t}</p></div>"), &sec(STUDENT), "");
    let note = |t: &str| page(&sec_note(t));
    let extra = |t: &str| page(&sec_extra(t));
    vec![
        // ── (b) conditional exemption as a bypass ──
        first("B1", "'당사 사정에 따라 본 이벤트를 조기 종료합니다.' (따라)", top("당사 사정에 따라 본 이벤트를 조기 종료합니다."), Expect::Gone),
        first("B2", "'이벤트는 종료합니다(재고 소진 시 조기 종료될 수 있음)'", note("이벤트는 종료합니다(재고 소진 시 조기 종료될 수 있음)"), Expect::Gone),
        first("B3", "badge '선착순 마감' (closed, first-come quota filled)", extra("<span class=\"badge\">선착순 마감</span>"), Expect::Gone),
        first("B4", "'이벤트 종료 (문의 사항이 있을 경우 고객센터로 연락 바랍니다)'", top("이벤트 종료 (문의 사항이 있을 경우 고객센터로 연락 바랍니다)"), Expect::Gone),
        first("B5", "'조기 마감합니다.다음 이벤트는 11월 중 오픈 예정입니다' (no space after .)", top("조기 마감합니다.다음 이벤트는 11월 중 오픈 예정입니다"), Expect::Gone),
        first("B6", "'판매 종료, 재입고 시 알림 신청 가능' (comma, standalone 시)", top("판매 종료, 재입고 시 알림 신청 가능"), Expect::Gone),
        first("B7", "'This offer has ended; if you have questions, contact us.'", top("This offer has ended; if you have questions, contact us."), Expect::Gone),
        first("B8", "'Sold out - restock dates may vary'", top("Sold out - restock dates may vary"), Expect::Gone),
        first("B9", "'당사 사정에 따라 2026년 10월 13일부로 종료합니다'", top("본 이벤트는 당사 사정에 따라 2026년 10월 13일부로 종료합니다"), Expect::Gone),
        first("B10", "'이벤트가 종료되어 지금은 응모하실 수 있는 기간이 아닙니다' (수 있)", top("이벤트가 종료되어 지금은 응모하실 수 있는 기간이 아닙니다"), Expect::Gone),
        first("B11", "control: '10월 13일 종료 예정' (past date)", note("10월 13일 종료 예정"), Expect::Gone),
        first("B12", "control: '재고 소진 시 조기 종료될 수 있습니다' (genuine condition)", note("재고 소진 시 조기 종료될 수 있습니다"), Expect::Same),
        // ── (c) label skipping ──
        first("C1", "only signal: <button disabled>신청 마감</button>", extra("<button type=\"button\" class=\"btn\" disabled>신청 마감</button>"), Expect::Gone),
        first("C2", "only signal: <a class=btn_end>이벤트 종료</a>", extra("<a href=\"#none\" class=\"btn_end\">이벤트 종료</a>"), Expect::Gone),
        first("C3", "control: <span class=badge>종료</span> (not a control)", extra("<span class=\"badge\">종료</span>"), Expect::Gone),
        first("C4", "clickable card <a> (<=20 chars) with badge 마감 and the offer", page("<ul class=\"cards\"><li><a href=\"/event/12\" class=\"card\"><span class=\"badge\">마감</span><p>학생 인증 시 10% 할인</p></a></li></ul>"), Expect::Gone),
        first("C5", "section heading as a link '마감된 혜택' above the offer", page(&format!("<h2><a href=\"/benefit?status=end\">마감된 혜택</a></h2>{}", sec(STUDENT))), Expect::Gone),
        first("C6", "selected tab '종료' and the offer in the active panel", page(&format!("<div class=\"tabs\" role=\"tablist\"><button role=\"tab\">진행중</button><button role=\"tab\" aria-selected=\"true\" class=\"on\">종료</button></div><div class=\"tab-pane active\" role=\"tabpanel\">{}</div>", sec(STUDENT))), Expect::Gone),
        first("C7", "<div role=button>마감</div> status", extra("<div role=\"button\" class=\"status\">마감</div>"), Expect::Gone),
        first("C8", "<a> wrapping <img alt='종료된 이벤트입니다'>", extra("<a href=\"#none\"><img src=\"/img/end.png\" alt=\"종료된 이벤트입니다\"></a>"), Expect::Gone),
        // ── (e) date scan at first sight (today 2026-10-18) ──
        first("E1", "'Offer valid until 03/31/2025' (year after the slash dropped, read 2027-03-31)", top("Offer valid until 03/31/2025"), Expect::Gone),
        first("E2", "'~26/10/13까지' (YY/MM/DD)", note("이벤트 기간 ~26/10/13까지"), Expect::Gone),
        first("E3", "'2026.10.13 이벤트 종료'", note("2026.10.13 이벤트 종료"), Expect::Gone),
        first("E4", "'2026년 10월 13일을 끝으로 종료'", note("2026년 10월 13일을 끝으로 종료"), Expect::Gone),
        first("E5", "'10. 13.(화) 오후 6시 마감'", note("10. 13.(화) 오후 6시 마감"), Expect::Gone),
        first("E6", "'Valid until 12/10/2026' (UK 12 Oct; US 10 Dec) ambiguous", top("Valid until 12/10/2026"), Expect::Gone),
        first("E7", "probe: 1차 ended + 2차 running '1차 2026.09.01~09.30 / 2차 2026.10.01~2026.12.31'", note("1차 2026.09.01~09.30 / 2차 2026.10.01~2026.12.31"), Expect::Same),
        first("E8", "control: range whose start is past '2026.10.01 ~ 2026.12.31'", note("이벤트 기간 2026.10.01 ~ 2026.12.31"), Expect::Same),
        first("E9", "probe: reward shipping '2026년 11월 10일 이후 순차 발송'", note("경품은 2026년 11월 10일 이후 순차 발송됩니다"), Expect::Same),
        // ── (g) first sight, ended in a form the scan misses ──
        first("G1", "'선착순 모집이 완료되었습니다.'", top("선착순 모집이 완료되었습니다."), Expect::Gone),
        first("G2", "'혜택 제공이 끝난 이벤트입니다'", top("혜택 제공이 끝난 이벤트입니다"), Expect::Gone),
        first("G3", "top bar '지금은 신청 기간이 아닙니다.' (negation read only in section)", top("지금은 신청 기간이 아닙니다."), Expect::Gone),
        first("G4", "'응모가 마무리되었습니다'", top("응모가 마무리되었습니다. 참여해 주셔서 감사합니다."), Expect::Gone),
        first("G5", "'접수가 모두 끝나 신청하실 수 없습니다'", top("접수가 모두 끝나 신청하실 수 없습니다"), Expect::Gone),
        first("G6", "'This promotion has finished.'", top("This promotion has finished."), Expect::Gone),
        first("G7", "'모집 완료' badge", extra("<span class=\"badge\">모집 완료</span>"), Expect::Gone),
        first("G8", "'혜택 소멸' / '이벤트 종료일이 지났습니다' control", top("이벤트 종료일이 지났습니다"), Expect::Gone),
        // ── false-hide probes: ordinary page furniture (expect confirm) ──
        first("H1", "<dl><dt>마감일</dt><dd>2026.12.31</dd>", extra("<dl class=\"info\"><dt>마감일</dt><dd>2026.12.31</dd></dl>"), Expect::Same),
        first("H2", "<th>이벤트 종료일</th><td>2026.12.31</td>", extra("<table><tr><th>이벤트 종료일</th><td>2026.12.31</td></tr></table>"), Expect::Same),
        first("H3", "<h3>취소 및 환불 안내</h3>", extra("<h3>취소 및 환불 안내</h3><p>결제일로부터 7일 이내 요청하실 수 있습니다.</p>"), Expect::Same),
        first("H4", "'결제 후 주문 취소는 마이페이지에서 가능합니다'", note("결제 후 주문 취소는 마이페이지에서 가능합니다"), Expect::Same),
        first("H5", "badge '마감 D-5'", extra("<span class=\"badge\">마감 D-5</span>"), Expect::Same),
        first("H6", "footer '더 이상 메일 수신을 원하지 않으시면 수신거부를 눌러주세요'", page3("", &sec(STUDENT), "<div class=\"mail\"><p>더 이상 메일 수신을 원하지 않으시면 수신거부를 눌러주세요</p></div>"), Expect::Same),
        first("H7", "<h3>포인트 유효기간 만료 안내</h3>", extra("<h3>포인트 유효기간 만료 안내</h3><p>적립일로부터 1년간 사용 가능합니다.</p>"), Expect::Same),
        first("H8", "sidebar: another event '추석 이벤트 ~2026.09.30'", page(&format!("{}<aside><h3>지난 소식</h3><p>추석 이벤트 ~2026.09.30</p></aside>", sec(STUDENT))), Expect::Same),
        first("H9", "notice '2026년 11월 1일부터 개정 약관 적용'", page3("<div class=\"top-notice\"><p>2026년 11월 1일부터 개정 약관이 적용됩니다.</p></div>", &sec(STUDENT), ""), Expect::Same),
        first("H10", "<h3>자동결제 해지·중단 방법</h3>", extra("<h3>자동결제 해지·중단 방법</h3><p>마이페이지에서 설정할 수 있습니다.</p>"), Expect::Same),
        first("H11", "'신청 마감까지 3일 남았어요'", extra("<p class=\"remain\">신청 마감까지 3일 남았어요</p>"), Expect::Same),
        first("H12", "<th>판매 종료</th><td>상시 판매</td>", extra("<table><tr><th>판매 종료</th><td>상시 판매</td></tr></table>"), Expect::Same),
        // ── (f) JSON-LD at first sight ──
        first("F7", "control: JSON-LD validThrough '2026-10-13T23:59:59+09:00'", page(&format!("{}{}", ld("{\"@type\":\"Offer\",\"validThrough\":\"2026-10-13T23:59:59+09:00\"}"), sec(STUDENT))), Expect::Gone),
        first("F8", "JSON-LD availability Discontinued at first sight", page(&format!("{}{}", ld("{\"@type\":\"Offer\",\"availability\":\"https://schema.org/Discontinued\"}"), sec(STUDENT))), Expect::Gone),
    ]
}

#[test]
fn r7_adversarial_replay() {
    let mut rows = Vec::new();
    for k in changed_cases() {
        let v = match seed(&k.before, k.quote, k.seed_at) {
            Err(why) => { println!("R7 {} SeedFailed | {}: {why}", k.id, k.name); Verdict::SeedFailed }
            Ok(s) => {
                let r = after(&s, &k.after, k.after_at);
                let v = judge(k.expect, &r);
                println!("R7 {} {v:?} | {} | expect {:?} | got {}: {r:?}", k.id, k.name, k.expect, r.label());
                v
            }
        };
        rows.push((k.id, v));
    }
    for k in first_cases() {
        let r = resolve(&view_at(&k.html, today()), &fact(k.quote, None, &Anchors::default(), true));
        let v = judge(k.expect, &r);
        println!("R7 {} {v:?} | first sight {} | expect {:?} | got {}: {r:?}", k.id, k.name, k.expect, r.label());
        rows.push((k.id, v));
    }
    let mut counts = std::collections::BTreeMap::new();
    for (_, v) in &rows { *counts.entry(*v).or_insert(0usize) += 1; }
    println!("R7 TOTAL {} verdicts: {counts:?}", rows.len());
}

/// D1 timeline: one unchanged page with a yearless end date, replayed
/// monthly. Shows the fact hides after the end, then confirms again.
#[test]
fn r7_yearless_rollover_timeline() {
    let html = page(&sec_note("이벤트 기간 ~ 3월 31일까지"));
    let s = seed(&html, STUDENT, d(2026, 3, 1)).expect("seed");
    for (m, day) in [(3, 31), (4, 1), (6, 1), (9, 28), (9, 30), (10, 1), (10, 18), (12, 31), (2027, 0)] {
        let at = if m == 2027 { d(2027, 3, 1) } else { d(2026, m, day) };
        let r = after(&s, &html, at);
        println!("R7 T {at}: {} {}", r.label(), if r.confirms() { "(shown)" } else { "(hidden)" });
    }
}

/// Digest equality, directly: pairs that differ in meaning.
#[test]
fn r7_digest_collisions() {
    let pairs = [
        ("댓글 3개 이상", "댓글 10개 이상"),
        ("친구 추천 3명", "친구 추천 5명"),
        ("D-3", "D+3"),
        ("D+30일 이내", "D+7일 이내"),
        ("이용 2시간 전까지", "이용 24시간 전까지"),
        ("등록일 2026.01.01 이후 가입", "등록일 2026.06.01 이후 가입"),
        ("1,2차", "12차"),
        ("1,000원", "1000원"),
        ("2. 5% 적립", "2.5% 적립"),
    ];
    for (a, b) in pairs {
        let da = view_at(&page(&sec_note(a)), today()).page_digest().to_string();
        let db = view_at(&page(&sec_note(b)), today()).page_digest().to_string();
        println!("R7 H {a:?} vs {b:?}: {}", if da == db { "SAME digest" } else { "different" });
    }
}
