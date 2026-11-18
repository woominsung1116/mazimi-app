//! Round-6 independent adversarial replay (reviewer-written). Same harness
//! as r5: seed (adopted exact confirm) -> confirm again with captured
//! anchors -> resolve the after-page with the live baseline and anchors.
//! Public API only, plus `neighbour`/`changed_only_by` for the dependent
//! path and `values::UNIT_VALUE` (pub(crate)) for the title rewrite probe.

use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::resilience::{
    capture, changed_only_by, neighbour, resolve, Anchors, PageView, Resolution, StoredFact,
};

const NO_AGES: Ages = Ages { min: None, max: None };
const STUDENT: &str = "학생 인증 시 10% 할인";
const FILLER: &str = "본 혜택은 제휴사 사정에 따라 변경될 수 있으며 자세한 내용은 고객센터로 문의해 주시기 바랍니다.";

fn filler(n: usize) -> String { (0..n).map(|_| FILLER).collect::<Vec<_>>().join(" ") }

/// `top` sits at body level before the header (띠배너), `bottom` at body
/// level after main (modals, layers).
fn page3(top: &str, main: &str, bottom: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>혜택 안내</title></head>\
         <body>{top}<header class=\"gnb\"><a href=\"/\">홈</a> <a href=\"/benefit\">혜택</a></header>\
         <main id=\"content\">{main}</main>{bottom}<footer class=\"ft\"><p>고객센터 1588-1234</p></footer></body></html>"
    )
}
fn page(main: &str) -> String { page3("", main, "") }
fn view(html: &str) -> PageView { PageView::new(html, &visible_text(html)) }

struct Stored { quote: String, baseline: String, anchors: Anchors }

fn fact<'a>(q: &'a str, b: Option<&'a str>, a: &'a Anchors, open: bool) -> StoredFact<'a> {
    StoredFact { quote: q, baseline: b, ages: NO_AGES, adoption_open: open, anchors: a }
}

fn seed(html: &str, quote: &str) -> Result<Stored, String> {
    let v = view(html);
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

fn after(s: &Stored, html: &str) -> Resolution {
    resolve(&view(html), &fact(&s.quote, Some(&s.baseline), &s.anchors, false))
}

#[derive(Clone, Copy, Debug)]
enum Expect { Same, Gone, Update(&'static str) }

#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
enum Verdict { Confirmed, Hidden, CorrectUpdate, FalseHide, MissedUpdate, FalseConfirm, WrongUpdate, SeedFailed }

fn judge(e: Expect, r: &Resolution) -> Verdict {
    let p = match r { Resolution::Proposed(p) => Some(p.new_signature.as_str()), _ => None };
    match e {
        Expect::Same if r.confirms() => Verdict::Confirmed,
        Expect::Same if p.is_some() => Verdict::WrongUpdate,
        Expect::Same => Verdict::FalseHide,
        Expect::Gone if r.confirms() => Verdict::FalseConfirm,
        Expect::Gone if p.is_some() => Verdict::WrongUpdate,
        Expect::Gone => Verdict::Hidden,
        Expect::Update(_) if r.confirms() => Verdict::FalseConfirm,
        Expect::Update(w) => match p { Some(g) if g == w => Verdict::CorrectUpdate, Some(_) => Verdict::WrongUpdate, None => Verdict::MissedUpdate },
    }
}

struct Case { id: &'static str, name: &'static str, quote: &'static str, before: String, after: String, expect: Expect }
fn c(id: &'static str, name: &'static str, quote: &'static str, before: String, after: String, expect: Expect) -> Case {
    Case { id, name, quote, before, after, expect }
}

fn sec(offer: &str) -> String {
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{offer}</p><p class=\"desc\">인증은 매 학기 갱신됩니다.</p></section>")
}
fn sec_note(note: &str) -> String {
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><p class=\"note\">{note}</p></section>")
}
fn intro() -> String { format!("<div class=\"intro\"><p>{}</p></div>", filler(4)) }

fn star_items(a: &str, b: &str, n1: &str, n2: &str) -> String {
    format!("<section><h2>신학기 학생 할인</h2><ul class=\"items\"><li>노트북 5% 할인{a}</li><li>태블릿 10% 할인{b}</li></ul>\
             <ul class=\"notes\"><li>{n1}</li><li>{n2}</li></ul></section>")
}

fn star_p(n1: &str, n2: &str) -> String {
    format!("<section><h2>신학기 학생 할인</h2><ul class=\"items\"><li>노트북 5% 할인*</li><li>태블릿 10% 할인**</li></ul>\
             <p class=\"fn\">{n1}</p><p class=\"fn\">{n2}</p></section>")
}

fn grid(heads: [&str; 3]) -> String {
    let h: String = heads.iter().map(|t| format!("<div class=\"col-head\">{t}</div>")).collect();
    format!("<section><h2>등급별 적립</h2><div class=\"compare\">{h}<div class=\"cell\">결제 금액의 10% 적립</div>\
             <div class=\"cell\">결제 금액의 5% 적립</div><div class=\"cell\">결제 금액의 1% 적립</div></div></section>")
}

fn accordion(target: &str) -> String {
    format!("<section><h2>학생 할인</h2><h3 class=\"acc-tit\">지원 대상 : {target}</h3><div class=\"acc-body\"><p>재학 증명서 또는 학생증으로 인증합니다.</p></div>\
             <h3 class=\"acc-tit\">혜택 내용</h3><div class=\"acc-body\"><p class=\"offer\">{STUDENT}</p></div></section>")
}

fn long_terms(extra: &str) -> String {
    let items: String = (1..=90)
        .map(|i| format!("<li>안내 {i}번 항목: 혜택 이용 방법과 인증 절차는 제휴사 운영 정책을 따릅니다</li>"))
        .collect();
    format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><div class=\"terms\"><h3>유의사항</h3><ul>{items}{extra}</ul></div></section>")
}

fn shared_footnotes(n1: &str, adjacent: bool) -> String {
    let a = "<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 10% 할인※1</p></section>";
    let b = "<section><h2>교직원 할인</h2><p class=\"offer\">교직원 인증 시 5% 할인※2</p></section>";
    let f = format!("<div class=\"footnotes\"><p>{n1}</p><p>※2 매장 결제 시 적용</p></div>");
    if adjacent { format!("{a}{f}{b}") } else { format!("{a}{b}{f}") }
}

fn tabs(label: &str) -> String {
    format!("<div class=\"tabs\" role=\"tablist\"><button role=\"tab\" aria-selected=\"true\" class=\"on\">{label}</button>\
             <button role=\"tab\">교직원</button></div><div class=\"tab-pane active\" role=\"tabpanel\">{}</div>", sec(STUDENT))
}

const MODAL_BTN: &str = "<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 10% 할인</p><button type=\"button\" data-target=\"#terms\">유의사항 보기</button></section>";
fn modal(text: &str) -> String {
    format!("<div class=\"modal\" id=\"terms\" role=\"dialog\"><div class=\"modal-body\"><strong>유의사항</strong><p>{text}</p></div></div>")
}

fn layer(text: &str) -> String {
    format!("<div class=\"layer-popup\" id=\"terms\" role=\"dialog\"><div class=\"layer-body\"><strong>이벤트 안내</strong><p>{text}</p></div></div>")
}

fn cases() -> Vec<Case> {
    vec![
        // ── controls ──
        c("K1", "control: unchanged page with body-level top bar", STUDENT,
          page3("<div class=\"top-notice\"><p>추석 연휴 배송 안내</p></div>", &sec(STUDENT), ""),
          page3("<div class=\"top-notice\"><p>추석 연휴 배송 안내</p></div>", &sec(STUDENT), ""), Expect::Same),
        c("K2", "control: in-section note '이벤트가 종료되었습니다.'", STUDENT,
          page(&sec_note("인증은 매 학기 갱신됩니다.")), page(&sec_note("이벤트가 종료되었습니다.")), Expect::Gone),
        c("K3", "control: footnote block right after the section, ※1 now ended", STUDENT,
          page(&shared_footnotes("※1 온라인 결제 시 적용", true)), page(&shared_footnotes("※1 이벤트가 종료되었습니다", true)), Expect::Gone),
        c("K4", "control: notice in main with an h1 page title above (root = main)", STUDENT,
          page(&format!("<h1>제휴 혜택</h1>{}", sec(STUDENT))),
          page(&format!("<h1>제휴 혜택</h1><div class=\"notice-box\"><p>※ 학생 할인은 종료되었습니다.</p></div>{}", sec(STUDENT))), Expect::Gone),
        // ── A: meaning changed outside the section scope ──
        c("A1", "body-level top bar '학생 할인 이벤트가 종료되었습니다.' (listed word)", STUDENT,
          page(&sec(STUDENT)), page3("<div class=\"top-notice\"><p>학생 할인 이벤트가 종료되었습니다.</p></div>", &sec(STUDENT), ""), Expect::Gone),
        c("A2", "body-level top bar, unlisted wording '모든 대학생 혜택 운영을 마쳤습니다'", STUDENT,
          page(&sec(STUDENT)), page3("<div class=\"top-notice\"><p>모든 대학생 대상 혜택 운영을 마쳤습니다. 그동안 성원에 감사드립니다.</p></div>", &sec(STUDENT), ""), Expect::Gone),
        c("A3", "conditions modal at body end: '※ 온라인 결제 시 적용' -> '※ 이벤트가 종료되었습니다'", STUDENT,
          page3("", MODAL_BTN, &modal("※ 온라인 결제 시 적용")), page3("", MODAL_BTN, &modal("※ 이벤트가 종료되었습니다")), Expect::Gone),
        c("A3b", "layer popup at body end titled '이벤트 안내': text -> '이벤트가 종료되었습니다'", STUDENT,
          page3("", MODAL_BTN, &layer("온라인 결제 시 적용")), page3("", MODAL_BTN, &layer("이벤트가 종료되었습니다")), Expect::Gone),
        c("A4", "shared footnote list after the next section: ※1 -> '이벤트가 종료되었습니다'", STUDENT,
          page(&shared_footnotes("※1 온라인 결제 시 적용", false)), page(&shared_footnotes("※1 이벤트가 종료되었습니다", false)), Expect::Gone),
        c("A5", "notice box in main before the section (no h1): '종료되었습니다'", STUDENT,
          page(&sec(STUDENT)),
          page(&format!("<div class=\"notice-box\"><p>※ 학생 할인은 종료되었습니다.</p></div>{}", sec(STUDENT))), Expect::Gone),
        c("A6", "tab label outside the section: 재학생 -> 신입생", STUDENT,
          page(&tabs("재학생")), page(&tabs("2026학년도 신입생")), Expect::Gone),
        c("A7", "hero banner (outside section, >150 chars away) now says 15%", STUDENT,
          page3("", &format!("{}{}", intro(), sec(STUDENT)), ""),
          page3("<div class=\"hero\"><p>학생 인증 시 15% 할인</p></div>", &format!("{}{}", intro(), sec(STUDENT)), ""), Expect::Gone),
        c("A8", "value 10%->15% with a top bar '변경된 할인율은 2026년 11월 1일부터 적용됩니다.'", STUDENT,
          page3("", &format!("{}{}", intro(), sec(STUDENT)), ""),
          page3("<div class=\"top-notice\"><p>변경된 할인율은 2026년 11월 1일부터 적용됩니다.</p></div>", &format!("{}{}", intro(), sec("학생 인증 시 15% 할인")), ""), Expect::Gone),
        c("A9", "image-only banner swapped ongoing -> closed, alt unchanged", STUDENT,
          page(&sec(&format!("{STUDENT}</p><p><img src=\"/img/student_event_on.png\" alt=\"학생 할인 이벤트\">"))),
          page(&sec(&format!("{STUDENT}</p><p><img src=\"/img/student_event_closed.png\" alt=\"학생 할인 이벤트\">"))), Expect::Gone),
        c("A10", "JSON-LD validThrough 2026-12-31 -> 2026-10-13, visible text unchanged", STUDENT,
          page(&format!("<script type=\"application/ld+json\">{{\"@type\":\"Offer\",\"validThrough\":\"2026-12-31\"}}</script>{}", sec(STUDENT))),
          page(&format!("<script type=\"application/ld+json\">{{\"@type\":\"Offer\",\"validThrough\":\"2026-10-13\"}}</script>{}", sec(STUDENT))), Expect::Gone),
        // ── B: in-section edits the digest does not see ──
        c("B1", "note: cap and minimum spend swapped (최대 5,000원 / 30,000원 이상)", STUDENT,
          page(&sec_note("1회 최대 5,000원 할인 · 30,000원 이상 결제 시")), page(&sec_note("1회 최대 30,000원 할인 · 5,000원 이상 결제 시")), Expect::Gone),
        c("B2", "note: percent and won swapped (3,000원 할인 / 5% 적립)", STUDENT,
          page(&sec_note("추가 3,000원 할인과 포인트 5% 적립")), page(&sec_note("추가 5% 할인과 포인트 3,000원 적립")), Expect::Gone),
        c("B3", "note: months swapped (첫 3개월 무료, 이후 12개월 약정)", STUDENT,
          page(&sec_note("첫 3개월 무료, 이후 12개월 약정")), page(&sec_note("첫 12개월 무료, 이후 3개월 약정")), Expect::Gone),
        c("B4", "asterisk footnotes reassigned: '* 온라인 전용' <-> '** 매장 전용'", "노트북 5% 할인",
          page(&star_items("*", "**", "* 온라인 스토어 전용", "** 오프라인 매장 전용")),
          page(&star_items("*", "**", "* 오프라인 매장 전용", "** 온라인 스토어 전용")), Expect::Gone),
        c("B4b", "asterisk footnotes as sibling <p>s reassigned: '* 온라인' <-> '** 매장'", "노트북 5% 할인",
          page(&star_p("* 온라인 스토어 전용", "** 오프라인 매장 전용")),
          page(&star_p("* 오프라인 매장 전용", "** 온라인 스토어 전용")), Expect::Gone),
        c("B5", "asterisk markers on items swapped (노트북 * -> **)", "노트북 5% 할인",
          page(&star_items("*", "**", "* 온라인 스토어 전용", "** 오프라인 매장 전용")),
          page(&star_items("**", "*", "* 온라인 스토어 전용", "** 오프라인 매장 전용")), Expect::Gone),
        c("B6", "※ / ※※ markers on items swapped", "노트북 5% 할인",
          page(&star_items("※", "※※", "※ 온라인 스토어 전용", "※※ 오프라인 매장 전용")),
          page(&star_items("※※", "※", "※ 온라인 스토어 전용", "※※ 오프라인 매장 전용")), Expect::Gone),
        c("B7", "values swapped across levels (sibling '최대 5,000원' <-> footnote '30,000원 이상')", STUDENT,
          page(&format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><p class=\"limit\">최대 5,000원</p></section><div class=\"footnotes\"><p>※ 30,000원 이상 결제 시</p></div>")),
          page(&format!("<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><p class=\"limit\">최대 30,000원</p></section><div class=\"footnotes\"><p>※ 5,000원 이상 결제 시</p></div>")), Expect::Gone),
        c("B8", "sibling accordion heading (not in the chain): 대학생·대학원생 -> 대학원생", STUDENT,
          page(&accordion("대학생·대학원생")), page(&accordion("대학원생")), Expect::Gone),
        c("B9", "flat grid: tier column heads reordered VIP,골드,일반 -> 골드,VIP,일반", "결제 금액의 10% 적립",
          page(&grid(["VIP", "골드", "일반"])), page(&grid(["골드", "VIP", "일반"])), Expect::Gone),
        c("B10", "long 유의사항 (>8000 bytes): unlisted restriction appended", STUDENT,
          page(&long_terms("")), page(&long_terms("<li>학생 인증은 국내 대학 학부 재학생만 이용할 수 있습니다</li>")), Expect::Gone),
        c("B11", "long 유의사항 (>8000 bytes): '본 이벤트는 2026년 10월 15일 종료되었습니다' appended", STUDENT,
          page(&long_terms("")), page(&long_terms("<li>본 이벤트는 2026년 10월 15일 종료되었습니다</li>")), Expect::Gone),
        // ── updates (correctness of proposals) ──
        c("U1", "control: value 10% -> 15%, nothing else", STUDENT,
          page(&sec(STUDENT)), page(&sec("학생 인증 시 15% 할인")), Expect::Update("15%")),
        c("U2", "value 1만 5천원 -> 1만 8천원 (proposal reads only the 천원 part)", "학생 인증 시 1만 5천원 할인",
          page(&sec("학생 인증 시 1만 5천원 할인")), page(&sec("학생 인증 시 1만 8천원 할인")), Expect::Update("18000원")),
        c("U3", "value 1만 원 -> 2만 원 (title-rewrite probe below)", "학생 인증 시 1만 원 할인",
          page(&sec("학생 인증 시 1만 원 할인")), page(&sec("학생 인증 시 2만 원 할인")), Expect::Update("2만원")),
    ]
}

fn campaign(period: &str) -> String {
    format!("<section><h2>가을 신학기 이벤트</h2><p class=\"period\">이벤트 기간: {period}</p><p class=\"offer\">{STUDENT}</p></section>")
}

/// Ended at first sight (today >= 2026-10-18): the seed must not confirm.
fn seed_cases() -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("S0", "control '2026.09.01 ~ 2026.10.13'", page(&campaign("2026.09.01 ~ 2026.10.13"))),
        ("S1", "two-digit year '26.09.01 ~ 26.10.13'", page(&campaign("26.09.01 ~ 26.10.13"))),
        ("S2", "'2026. 10. 13.(화)까지'", page(&campaign("2026. 10. 13.(화)까지"))),
        ("S3", "'2026.10.13.까지'", page(&campaign("2026.10.13.까지"))),
        ("S4", "'2026년 10월 13일(화) 18시까지'", page(&campaign("2026년 10월 13일(화) 18시까지"))),
        ("S5", "'10월 13일 자정까지'", page(&campaign("10월 13일 자정까지"))),
        ("S6", "U+223C '2026.09.01 ∼ 2026.10.13'", page(&campaign("2026.09.01 \u{223C} 2026.10.13"))),
        ("S7", "U+301C '2026.09.01 〜 2026.10.13'", page(&campaign("2026.09.01 \u{301C} 2026.10.13"))),
        ("S8", "English 'Sep 1 – Oct 13, 2026'", page(&campaign("Sep 1 – Oct 13, 2026"))),
        ("S9", "'종료' after 8000 bytes of the section", page(&long_terms("<li>본 이벤트는 종료되었습니다</li>"))),
    ]
}

#[test]
fn r6_adversarial_replay() {
    let mut rows = Vec::new();
    for k in cases() {
        let v = match seed(&k.before, k.quote) {
            Err(why) => { println!("R6 {} SeedFailed | {}: {why}", k.id, k.name); Verdict::SeedFailed }
            Ok(s) => {
                let r = after(&s, &k.after);
                let v = judge(k.expect, &r);
                println!("R6 {} {v:?} | {} | expect {:?} | got {}: {r:?}", k.id, k.name, k.expect, r.label());
                v
            }
        };
        rows.push((k.id, v));
    }
    for (id, name, html) in seed_cases() {
        let r = resolve(&view(&html), &fact(STUDENT, None, &Anchors::default(), true));
        let v = if r.confirms() { Verdict::FalseConfirm } else { Verdict::Hidden };
        println!("R6 {id} {v:?} | seed {name} | got {}", r.label());
        rows.push((id, v));
    }
    let mut counts = std::collections::BTreeMap::new();
    for (_, v) in &rows { *counts.entry(*v).or_insert(0usize) += 1; }
    println!("R6 TOTAL {} verdicts: {counts:?}", rows.len());
}

/// Dependent-fact follow rule across sections: B is in another section of
/// the same programme; its own section's condition changes 10,000원 ->
/// 15,000원 at the same time as A's value does.
#[test]
fn r6_dependent_follow_across_sections() {
    let html = |a: &str, cond: &str| page(&format!(
        "<section><h2>온라인 쇼핑</h2><p class=\"offer\">학생 인증 시 월 {a} 할인</p></section>{}\
         <section><h2>카페</h2><p class=\"offer\">카페 음료 20% 할인</p><p class=\"cond\">{cond} 이상 결제 시</p></section>",
        intro()));
    let before = html("10,000원", "10,000원");
    let after_html = html("15,000원", "15,000원");
    let a = seed(&before, "학생 인증 시 월 10,000원 할인").expect("seed A");
    let b = seed(&before, "카페 음료 20% 할인").expect("seed B");
    let v = view(&after_html);
    let ra = resolve(&v, &fact(&a.quote, Some(&a.baseline), &a.anchors, false));
    let rb = resolve(&v, &fact(&b.quote, Some(&b.baseline), &b.anchors, false));
    println!("R6 D1 A: {}: {ra:?}", ra.label());
    println!("R6 D1 B: {}: {rb:?}", rb.label());
    let (old, new) = match &ra { Resolution::Proposed(p) => (p.old_signature.clone(), p.new_signature.clone()), _ => (String::new(), String::new()) };
    let nb = neighbour(&v, &fact(&b.quote, Some(&b.baseline), &b.anchors, false));
    let stored = &b.anchors.fingerprint.as_ref().unwrap().identities[0];
    let follows = nb.as_ref().is_some_and(|n| changed_only_by(stored, n, &old, &new));
    let swap = |sig: &str| -> String {
        let mut parts: Vec<String> = sig.split('|').map(|p| {
            let mut t: Vec<&str> = p.split(',').filter(|t| !t.is_empty()).map(|t| if t == old { new.as_str() } else { t }).collect();
            t.sort(); t.join(",")
        }).collect();
        parts.sort(); parts.join("|")
    };
    let window_follows = nb.as_ref().and_then(|n| n.window.clone()).is_some_and(|w| swap(&b.baseline) == w || b.baseline == w);
    println!("R6 D1 B follows A's change ({old}->{new}): changed_only_by={follows} window_follows={window_follows} -> {}",
        if follows && window_follows && !rb.confirms() { "FalseConfirm (re-baselined + confirmed once A is applied live)" } else { "Hidden" });
}

/// The title rewrite in `evidence::replace_value` builds its token without
/// dropping the space in `만 원`/`천 원`, while `value_signature` drops it.
#[test]
fn r6_title_rewrite_token_mismatch() {
    use crate::corporate_auto::values::{value_signature, UNIT_VALUE};
    for (title, quote) in [("학생 1만 원 할인", "학생 인증 시 1만 원 할인"), ("학생 월 1만원 할인", "학생 인증 시 월 10,000원 할인")] {
        let old = value_signature(quote);
        // Same filter as evidence.rs replace_value.
        let hits = UNIT_VALUE.captures_iter(title)
            .filter(|c| format!("{}{}", c[1].replace(',', ""), c[2].to_lowercase()) == old).count();
        println!("R6 T title {title:?} old token {old:?}: replace_value hits {hits} -> {}",
            if hits == 0 { "title NOT rewritten, fact applied: stale public value" } else { "rewritten" });
    }
}
