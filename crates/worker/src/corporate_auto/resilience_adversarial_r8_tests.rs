//! Round-8 independent adversarial review (reviewer-written) of d571ce27.
//! Mounted under `resilience::tests` (reviewer checkout only, never
//! committed). Every page is read at an injected KST moment
//! (`PageView::at`), so nothing depends on the wall clock.
//!
//! Part 1c: precision of the default hide_only reasons on active pages.
//! Part 2: new cases against the r8 gates (0 false confirms, 0 wrong
//! updates): exclusions, label/value pairing, countdowns, footer/aside/terms
//! skips, first-sight dates, v1->v2 re-adoption, publish field checks.

use crate::corporate_auto::bootstrap::extract_offer;
use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::publish::{page_blocked_scan, verify_public_fields, PublicProgramme, ScanVerdict};
use crate::corporate_auto::resilience::{
    capture, extractors_agree, page_blocked, reason_key, resolve, Anchors, PageView, Resolution, StoredFact,
};
use chrono::{NaiveDate, NaiveDateTime};

const NO_AGES: Ages = Ages { min: None, max: None };
const Q: &str = "학생 인증 시 10% 할인";

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
fn noon(day: NaiveDate) -> NaiveDateTime { day.and_hms_opt(12, 0, 0).unwrap() }
fn today() -> NaiveDate { d(2026, 10, 18) }
fn view(html: &str, at: NaiveDate) -> PageView { PageView::at(html, &visible_text(html), noon(at)) }

/// A realistic Korean brand page: logo h1 in the header, GNB, the offer in
/// main, a footer with policy links.
fn page_with(body_attr: &str, top: &str, main: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>학생 혜택 | 브랜드</title></head>\
         <body{body_attr}>{top}<header class=\"hd\"><h1 class=\"logo\"><a href=\"/\">브랜드</a></h1>\
         <nav class=\"gnb\"><ul><li><a href=\"/event\">진행 이벤트</a></li><li><a href=\"/event/past\">지난 이벤트</a></li></ul></nav></header>\
         <main id=\"content\">{main}</main>\
         <footer class=\"ft\"><a href=\"/terms\">이용약관</a> <a href=\"/privacy\">개인정보처리방침</a><p>고객센터 1588-1234</p></footer></body></html>"
    )
}
fn page(main: &str) -> String { page_with("", "", main) }
fn offer(extra: &str) -> String {
    page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p>{extra}</section>"))
}

fn fact<'a>(q: &'a str, b: Option<&'a str>, a: &'a Anchors, open: bool) -> StoredFact<'a> {
    StoredFact { quote: q, baseline: b, ages: NO_AGES, adoption_open: open, anchors: a }
}

/// First sight: an audit seed adopted, a fact re-adopted, or a fresh
/// extraction (same reading through `page_blocked`).
fn first_sight(html: &str, quote: &str, at: NaiveDate) -> Resolution {
    resolve(&view(html, at), &fact(quote, None, &Anchors::default(), true))
}

struct Stored { quote: String, baseline: String, anchors: Anchors }

fn seed(html: &str, quote: &str, at: NaiveDate) -> Result<Stored, String> {
    let v = view(html, at);
    let empty = Anchors::default();
    let window = match resolve(&v, &fact(quote, None, &empty, true)) {
        Resolution::Exact(FactOutcome::Confirmed { window_signature, adopted: true }) => window_signature,
        other => return Err(format!("seed not adopted: {other:?}")),
    };
    let anchors = Anchors { fingerprint: capture(&v, quote), ..Anchors::default() };
    if anchors.fingerprint.is_none() { return Err("no holder".into()); }
    Ok(Stored { quote: quote.into(), baseline: window, anchors })
}

fn later(s: &Stored, html: &str, at: NaiveDate) -> Resolution {
    resolve(&view(html, at), &fact(&s.quote, Some(&s.baseline), &s.anchors, false))
}

fn key_of(r: &Resolution) -> String {
    match r {
        Resolution::Ambiguous(reason) => reason_key(reason).unwrap_or("other").to_string(),
        other => other.label().to_string(),
    }
}

const DEFAULT_ALLOW: &[&str] = &["section_date_past", "section_ended_past", "structured_ended", "source_gone"];

// ── Part 1c: precision of the default reasons on active offers ───────────

/// Active offers. Each line: `R8P <id> <verdict> <key> applied_in_hide_only=<bool> | <name>`.
#[test]
fn r8_part1_default_reason_precision() {
    let past_list = "<div class=\"event-list\"><h3>다른 이벤트</h3><ul>\
        <li><a href=\"/e/1\"><strong>여름방학 학생 특가</strong><span class=\"date\">2026.07.01 ~ 2026.08.31</span></a></li>\
        <li><a href=\"/e/2\"><strong>신학기 노트북 할인</strong><span class=\"date\">2026.02.01 ~ 2026.03.31</span></a></li></ul></div>";
    let ld_product = "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"name\":\"추천 상품 A\",\"offers\":{\"@type\":\"Offer\",\"price\":\"39000\",\"priceCurrency\":\"KRW\",\"priceValidUntil\":\"2025-12-31\"}}</script>";
    let ld_events = "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"ItemList\",\"itemListElement\":[{\"@type\":\"Event\",\"name\":\"지난 팝업 스토어\",\"startDate\":\"2026-08-01\",\"endDate\":\"2026-08-15\"}]}</script>";
    let micro_soldout = "<div class=\"related\" itemscope itemtype=\"https://schema.org/Product\"><span itemprop=\"name\">함께 보면 좋은 상품</span>\
        <div itemprop=\"offers\" itemscope itemtype=\"https://schema.org/Offer\"><link itemprop=\"availability\" href=\"https://schema.org/SoldOut\"><span>일시품절</span></div></div>";
    let cases: Vec<(&str, &str, String)> = vec![
        ("P1", "past-events list inside main (not an aside)", page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p><p>기간: 2026.10.01 ~ 2026.12.31</p></section>{past_list}"))),
        ("P1b", "past-events list inside the offer section", offer(&format!("<p>기간: 2026.10.01 ~ 2026.12.31</p>{past_list}"))),
        ("P2", "earlier phase of a two-phase offer", offer("<p>1차 신청 기간: 2026.09.01 ~ 2026.09.30</p><p>2차 신청 기간: 2026.10.01 ~ 2026.11.30</p>")),
        ("P3", "earlier phase closed (1차 모집 마감)", offer("<p>1차 모집 마감 / 2차 모집 진행 중 (~2026.11.30)</p>")),
        ("P4", "result announcement date passed (no 까지)", offer("<p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p><p>1차 당첨자 발표: 2026.10.10(토)</p>")),
        ("P4b", "reward shipping deadline passed (까지)", offer("<p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p><p>1차 당첨자 경품 발송: 10월 15일까지</p>")),
        ("P5", "board meta 등록일/수정일", offer("<p class=\"meta\">등록일 2026.09.01 | 수정일 2026.09.15</p>")),
        ("P6", "verification flow: 인증이 완료되었다면", offer("<p>학생 인증이 완료되었다면 쿠폰함을 확인하세요.</p>")),
        ("P7", "수능 끝난 수험생 (seasonal youth copy)", offer("<p>수능 끝난 수험생을 위한 특별 혜택도 함께 확인하세요.</p>")),
        ("P8", "인증을 마쳤다면", offer("<p>인증을 마쳤다면 바로 사용할 수 있어요.</p>")),
        ("P9", "기말고사 끝나고 (copy)", offer("<p>기말고사 끝나고 떠나는 여행도 학생 요금으로!</p>")),
        ("P10", "legacy-member note with a past date", offer("<p>※ 2025년 12월 31일까지 가입한 회원은 기존 혜택이 유지됩니다.</p>")),
        ("P11", "graduation cut-off with a past month", offer("<p>※ 2026년 2월까지 졸업(예정)한 학생도 대상입니다.</p>")),
        ("P12", "JSON-LD of a related product, stale priceValidUntil", page(&format!("{ld_product}<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>"))),
        ("P13", "JSON-LD ItemList of a past event", page(&format!("{ld_events}<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>"))),
        ("P14", "microdata of a sold-out related product", page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>{micro_soldout}"))),
        ("P15", "coupon validity relative to issue (control)", offer("<p>쿠폰 유효기간: 발급일로부터 30일</p>")),
        ("P16", "previous-year winners excluded", offer("<p>※ 전년도 이벤트(2025.10.01~2025.12.31) 당첨자는 참여할 수 없습니다.</p>")),
    ];
    let (mut hid, mut applied_n) = (0, 0);
    for (id, name, html) in &cases {
        let r = first_sight(html, Q, today());
        let key = key_of(&r);
        let applied = !r.confirms() && DEFAULT_ALLOW.contains(&key.as_str());
        hid += usize::from(!r.confirms());
        applied_n += usize::from(applied);
        let verdict = if r.confirms() { "Confirmed" } else { "FalseHide" };
        println!("R8P {id} {verdict} {key} applied_in_hide_only={applied} | {name}");
    }
    println!("R8P TOTAL cases={} false_hides={hid} applied_by_default_allowlist={applied_n}", cases.len());
}

// ── Part 2: false confirms and wrong updates ─────────────────────────────

#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
enum Verdict { Hidden, FalseConfirm, FalseHide, Confirmed, WrongUpdate, SeedFailed }

fn gone(r: &Resolution) -> Verdict {
    if matches!(r, Resolution::Proposed(_)) { Verdict::WrongUpdate }
    else if r.confirms() { Verdict::FalseConfirm } else { Verdict::Hidden }
}

/// Every case expects the offer not shown (it ended, is not on yet, or the
/// page no longer reads as before). `R8 <id> <verdict> <detail> | <name>`.
#[test]
fn r8_part2_adversarial() {
    let mut out: Vec<(String, Verdict, String, String)> = Vec::new();
    fn push(out: &mut Vec<(String, Verdict, String, String)>, id: &str, name: &str, r: Resolution) {
        let v = gone(&r);
        out.push((id.into(), v, key_of(&r), name.into()));
    }
    let t = today();

    // A. Ending + a classified future date in one clause: the ending is
    // "left to the date", the date is a different deadline.
    for (id, name, extra) in [
        ("A1", "조기 종료되어 + coupon use deadline", "<p>이벤트가 조기 종료되어 이미 발급된 쿠폰만 12월 31일까지 사용 가능합니다.</p>"),
        ("A2", "판매가 종료되어 + A/S deadline", "<p>판매가 종료되어 2026.12.31까지 A/S 접수만 가능합니다.</p>"),
        ("A3", "응모 마감 / shipping deadline", "<p>응모 마감 / 경품은 11월 30일까지 순차 발송됩니다.</p>"),
        ("A4", "종료되며 + coupon deadline", "<p>본 이벤트는 선착순 마감으로 종료되며 기 발급 쿠폰은 2026년 12월 31일까지 사용하실 수 있습니다.</p>"),
        ("A5", "dl status 마감 + shipping deadline (label pairing)", "<dl><dt>진행 상태</dt><dd>마감 12월 31일까지 경품 순차 발송</dd></dl>"),
    ] {
        push(&mut out, id, name, first_sight(&offer(extra), Q, t));
    }

    // B. Exclusion narrowing (first sight / fresh extraction).
    for (id, name, extra) in [
        ("B1", "본 혜택은 더 이상 제공하지 않습니다 (active voice)", "<ul class=\"terms\"><li>※ 본 혜택은 더 이상 제공하지 않습니다.</li></ul>"),
        ("B2", "본 쿠폰은 현재 지급되지 않습니다", "<ul class=\"terms\"><li>※ 본 쿠폰은 현재 지급되지 않습니다.</li></ul>"),
        ("B3", "generic clause swallows a subject-naming negation", "<ul class=\"terms\"><li>※ 타 할인과 중복 불가하며 학생 할인은 현재 적용되지 않습니다.</li></ul>"),
        ("B4", "subject-naming exclusion in a card-footer", "<div class=\"card-footer\"><p>※ 학생 할인은 현재 모든 매장에서 적용되지 않습니다.</p></div>"),
    ] {
        push(&mut out, id, name, first_sight(&offer(extra), Q, t));
    }

    // C. Countdowns.
    push(&mut out, "C1", "오픈 D-3 (not started)", first_sight(&offer("<p class=\"badge\">오픈 D-3</p>"), Q, t));
    push(&mut out, "C2", "dated opening without 부터 (10월 25일 오픈)", first_sight(&offer("<p>2026년 10월 25일(일) 그랜드 오픈!</p>"), Q, t));
    push(&mut out, "C3", "시작/종료 labelled cells, start ahead", first_sight(&offer("<p>이벤트 시작 2026.11.01 · 이벤트 종료 2026.11.30</p>"), Q, t));
    {
        // A countdown is not anchored to the first sight: seen 10-01 with
        // 마감 D-3, unchanged page read 10-18.
        let html = offer("<p class=\"dday\">응모 마감 D-3</p>");
        match seed(&html, Q, d(2026, 10, 1)) {
            Ok(s) => push(&mut out, "C4", "stale 마감 D-3 seen 10-01, unchanged on 10-18", later(&s, &html, t)),
            Err(e) => out.push(("C4".into(), Verdict::SeedFailed, e, "stale countdown".into())),
        }
    }

    // D. Footer/aside/terms skip as a hiding place.
    let card = |footer: &str| page(&format!(
        "<section class=\"evt\"><h2>이벤트</h2><div class=\"card\"><div class=\"card-body\"><p>{Q}</p></div><div class=\"card-footer\">{footer}</div></div></section>"
    ));
    push(&mut out, "D1", "card-footer says ended", first_sight(&card("<p>이벤트가 종료되었습니다.</p>"), Q, t));
    push(&mut out, "D2", "card-footer disabled CTA 신청 마감", first_sight(&card("<button type=\"button\" disabled>신청 마감</button>"), Q, t));
    {
        let html = page(&format!("<article><h2>학생 할인</h2><p>{Q}</p><footer>이벤트 기간: 2026.10.01 ~ 2026.10.31</footer></article>"));
        match seed(&html, Q, t) {
            Ok(s) => push(&mut out, "D3", "article footer period passes (unchanged page, 11-05)", later(&s, &html, d(2026, 11, 5))),
            Err(e) => out.push(("D3".into(), Verdict::SeedFailed, e, "article footer".into())),
        }
    }
    {
        let html = card("<span>기간: 2026.10.01 ~ 2026.10.31</span>");
        match seed(&html, Q, t) {
            Ok(s) => push(&mut out, "D4", "card-footer period passes (unchanged page, 11-05)", later(&s, &html, d(2026, 11, 5))),
            Err(e) => out.push(("D4".into(), Verdict::SeedFailed, e, "card footer".into())),
        }
    }
    let main_h1 = format!("<h1 class=\"tit\">학생 할인</h1><p>{Q}</p>");
    push(&mut out, "D5", "body.has-sidebar + top notice outside the section",
        first_sight(&page_with(" class=\"page has-sidebar\"", "<div class=\"top-notice\"><p>학생 할인 이벤트가 종료되었습니다.</p></div>", &main_h1), Q, t));
    push(&mut out, "D6", "top notice mentioning 약관 says the offer ended",
        first_sight(&page_with("", "<div class=\"top-notice\"><p>약관 개정에 따라 학생 할인 혜택이 종료되었습니다.</p></div>", &main_h1), Q, t));
    push(&mut out, "D7", "in-section 약관 clause carries the passed end date",
        first_sight(&offer("<p>이용약관 개정으로 학생 할인은 2026년 10월 15일 종료</p>"), Q, t));
    push(&mut out, "D8", "wrapper class content-with-sidebar hides an ending outside the section",
        first_sight(&page_with("", "", &format!("<div class=\"content-with-sidebar\"><div class=\"notice\"><p>여름 학생 할인이 종료되었습니다.</p></div><div><h1>학생 할인</h1><p>{Q}</p></div></div>")), Q, t));

    // E. First-sight dates.
    push(&mut out, "E1", "stale yearless 3월 31일(화) (2026 Tuesday) read as 2027", first_sight(&offer("<p>신학기 학생 할인 3월 31일(화)까지</p>"), Q, t));
    push(&mut out, "E2", "stale yearless ~2/28 (ambiguous year) read as 2027", first_sight(&offer("<p>신학기 맞이 ~ 2월 28일까지</p>"), Q, t));
    for (id, name, extra, seen, at) in [
        ("E3", "연말까지 (not a known relative deadline), unchanged into 2027", "<p>연말까지 학생 할인 진행</p>", t, d(2027, 1, 5)),
        ("E4", "월말까지, unchanged into next month", "<p>월말까지 학생 특가</p>", t, d(2026, 11, 5)),
        ("E5", "내일까지, unchanged a week later", "<p>내일까지만 학생 특가</p>", t, d(2026, 10, 25)),
        ("E6", "이번 학기 한정, unchanged next year", "<p>이번 학기 한정 학생 특가</p>", t, d(2027, 9, 1)),
    ] {
        let html = offer(extra);
        match seed(&html, Q, seen) {
            Ok(s) => push(&mut out, id, name, later(&s, &html, at)),
            Err(e) => out.push((id.into(), Verdict::SeedFailed, e, name.into())),
        }
    }

    // F. v1 -> v2 re-adoption window (stored digest of the old version).
    {
        let before = offer("<p>1인 최대 5,000원까지 할인</p>");
        let after = offer("<p>1인 최대 1,000원까지 할인</p>");
        match seed(&before, Q, d(2026, 10, 17)) {
            Ok(mut s) => {
                s.anchors.fingerprint.as_mut().unwrap().page = "v1:0123abcd".into();
                s.anchors.fingerprint.as_mut().unwrap().seen = None;
                let r = resolve(&view(&after, t), &fact(&s.quote, Some(&s.baseline), &s.anchors, true));
                push(&mut out, "F1", "cap 5,000원 -> 1,000원 in the window, laundered by v1 re-adoption", r);
                // control: the same edit under a v2 baseline hides.
                let mut s2 = seed(&before, Q, d(2026, 10, 17)).unwrap();
                s2.anchors.fingerprint.as_mut().unwrap().seen = Some(d(2026, 10, 17));
                push(&mut out, "F1c", "control: same edit under a v2 digest", later(&s2, &after, t));
            }
            Err(e) => out.push(("F1".into(), Verdict::SeedFailed, e, "v1 cap".into())),
        }
        let html = offer("<p>이번 달 말까지 진행</p>");
        match seed(&html, Q, d(2026, 9, 10)) {
            Ok(mut s) => {
                s.anchors.fingerprint.as_mut().unwrap().page = "v1:0123abcd".into();
                s.anchors.fingerprint.as_mut().unwrap().seen = None;
                let r = resolve(&view(&html, t), &fact(&s.quote, Some(&s.baseline), &s.anchors, true));
                push(&mut out, "F2", "이번 달 말까지 first seen 09-10 under v1, re-adopted 10-18 (first sight reset)", r);
            }
            Err(e) => out.push(("F2".into(), Verdict::SeedFailed, e, "v1 relative".into())),
        }
    }

    // K. Controls: the same pages without the mechanism under test must hide.
    for (id, name, html, at, seeded) in [
        ("K1", "control: plain past-tense ending in section", offer("<p>이벤트가 종료되었습니다.</p>"), t, None),
        ("K2", "control: plain past period in section", offer("<p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>"), t, None),
        ("K3", "control D5 without has-sidebar", page_with("", "<div class=\"top-notice\"><p>학생 할인 이벤트가 종료되었습니다.</p></div>", &main_h1), t, None),
        ("K4", "control D6 without 약관", page_with("", "<div class=\"top-notice\"><p>개편에 따라 학생 할인 혜택이 종료되었습니다.</p></div>", &main_h1), t, None),
        ("K5", "control D1 with card-bottom", page(&format!("<section class=\"evt\"><h2>이벤트</h2><div class=\"card\"><div class=\"card-body\"><p>{Q}</p></div><div class=\"card-bottom\"><p>이벤트가 종료되었습니다.</p></div></div></section>")), t, None),
        ("K6", "control D3 with <p> instead of <footer>", page(&format!("<article><h2>학생 할인</h2><p>{Q}</p><p>이벤트 기간: 2026.10.01 ~ 2026.10.31</p></article>")), d(2026, 11, 5), Some(t)),
        ("K7", "control E3 with 올해 말까지", offer("<p>올해 말까지 학생 할인 진행</p>"), d(2027, 1, 5), Some(t)),
        ("K8", "control A1 without the second deadline", offer("<p>이벤트가 조기 종료되어 쿠폰 발급이 중지됩니다.</p>"), t, None),
        ("K9", "control C3 with 부터 시작", offer("<p>2026.11.01부터 시작</p>"), t, None),
        ("K10", "control B3 split by a comma", offer("<ul class=\"terms\"><li>※ 타 할인과 중복 불가, 학생 할인은 현재 적용되지 않습니다.</li></ul>"), t, None),
        ("K11", "control D7 without 약관", offer("<p>제도 개편으로 학생 할인은 2026년 10월 15일 종료</p>"), t, None),
        ("K12", "control E1 with the year", offer("<p>신학기 학생 할인 2026년 3월 31일(화)까지</p>"), t, None),
    ] {
        let r = match seeded {
            None => first_sight(&html, Q, t),
            Some(seen) => match seed(&html, Q, seen) { Ok(s) => later(&s, &html, at), Err(e) => { out.push((id.into(), Verdict::SeedFailed, e, name.into())); continue; } },
        };
        push(&mut out, id, name, r);
    }

    // G. The same pages through fresh extraction's scan (page_blocked):
    // a Clean scan is the gate that lets publish run.
    for (id, extra) in [
        ("G-A1", "<p>이벤트가 조기 종료되어 이미 발급된 쿠폰만 12월 31일까지 사용 가능합니다.</p>"),
        ("G-B1", "<ul class=\"terms\"><li>※ 본 혜택은 더 이상 제공하지 않습니다.</li></ul>"),
        ("G-D7", "<p>이용약관 개정으로 학생 할인은 2026년 10월 15일 종료</p>"),
        ("G-E1", "<p>신학기 학생 할인 3월 31일(화)까지</p>"),
    ] {
        let html = offer(extra);
        let v = view(&html, t);
        let blocked = page_blocked(&v, Q);
        let verdict = if blocked.is_none() { Verdict::FalseConfirm } else { Verdict::Hidden };
        out.push((id.into(), verdict, blocked.and_then(reason_key).unwrap_or("clean").into(), "fresh-extraction scan".into()));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    let mut counts = std::collections::BTreeMap::new();
    for (id, v, key, name) in &out {
        *counts.entry(format!("{v:?}")).or_insert(0) += 1;
        println!("R8 {id} {v:?} {key} | {name}");
    }
    println!("R8 TOTAL verdicts={} {counts:?}", out.len());
}

// ── Part 2: publish field checks (pure chain of the live publish gates) ──

fn filler() -> String {
    "<section class=\"brand\"><h2>브랜드 소개</h2>".to_string()
        + &"<p>우리는 좋은 재료와 정직한 공정으로 매일 쓰는 물건을 만듭니다. 매장과 온라인 스토어에서 같은 품질을 경험하실 수 있습니다.</p>".repeat(12)
        + "</section>"
}

fn programme(title: &str, summary: &str, url: &str) -> PublicProgramme {
    PublicProgramme {
        program_type: "corporate_benefit".into(), title: title.into(), summary: Some(summary.into()),
        official_url: Some(url.into()), application_method: None, eligibility_condition: None, submission_documents: None,
        screening_method: None, application_steps: None, benefit_conditions: None,
        application_start_at: None, application_end_at: None, deadline_at: None, benefit_valid_until: None,
        benefit_amount_monthly: None, benefit_amount_semester: None, benefit_amount_once: None,
        min_age: None, max_age: None, regions: None, region_districts: None, region_scope: None,
        school_scope: None, tags: None,
    }
}

/// `R8W <id> <verdict> <gate> | <name>`: every gate the live publish runs on
/// the page (extract, ending scan, extractor agreement, public fields).
#[test]
fn r8_part2_publish_field_checks() {
    let url = "https://brand.example/student";
    let cases: Vec<(&str, &str, &str, &str, String)> = vec![
        ("W0p", "positive control: same offer, same page", "대학생 10% 할인", "대학생 10% 할인 혜택",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 10% 할인</p></section>{}", filler()))),
        ("W1", "values swapped between two audiences", "대학원생 10% 할인", "대학원생은 10% 할인됩니다.",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 10% 할인</p><p>대학원생 20% 할인</p></section>{}", filler()))),
        ("W2", "할인 became 적립 (same number)", "대학생 10% 할인", "대학생 10% 할인 혜택",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 혜택으로 결제 금액의 10% 적립</p></section>{}", filler()))),
        ("W3", "non-numeric claim gone (첫 달 무료)", "대학생 50% 할인 + 첫 달 무료", "대학생 50% 할인에 첫 달 무료",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 50% 할인</p></section>{}", filler()))),
        ("W4", "monthly became once (매월 -> 가입 시 1회)", "대학생 매월 5,000원 할인", "대학생 매월 5,000원 할인",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 가입 시 1회 5,000원 할인 쿠폰</p></section>{}", filler()))),
        ("W5", "audience narrowed (학생 -> 대학원생 only)", "학생 10% 할인", "학생이면 누구나 10% 할인",
            page(&format!("<section><h2>학생 혜택</h2><p>대학원생 전용 10% 할인</p></section>{}", filler()))),
        ("W6c", "control: cap changed 5,000원 -> 3,000원", "대학생 10% 할인 (최대 5,000원)", "",
            page(&format!("<section><h2>학생 혜택</h2><p>대학생 10% 할인 (최대 3,000원)</p></section>{}", filler()))),
    ];
    let mut wrong = 0;
    for (id, name, title, summary, html) in &cases {
        let gate = (|| -> Result<(), String> {
            let offer = extract_offer(html, today()).map_err(|e| format!("extract:{e}"))?;
            let quotes: Vec<&str> = offer.facts.iter().chain(offer.verification.iter()).map(|f| f.quote.as_str()).collect();
            if let ScanVerdict::Ended(r) = page_blocked_scan(html, &quotes) { return Err(format!("scan:{r}")); }
            if !quotes.iter().all(|q| extractors_agree(q)) { return Err("extractors_disagree".into()); }
            verify_public_fields(&programme(title, summary, url), &offer, &quotes, &visible_text(html), url)
        })();
        let verdict = match (&gate, id.ends_with('c'), id.ends_with('p')) {
            (Ok(()), _, true) => "Published(expected)",
            (Err(_), _, true) => "Refused(positive control failed!)",
            (Ok(()), false, _) => { wrong += 1; "WrongUpdate" }
            (Ok(()), true, _) => "WrongUpdate(control!)",
            (Err(_), _, _) => "Refused",
        };
        println!("R8W {id} {verdict} {} | {name}", gate.err().unwrap_or_else(|| "published".into()));
    }
    println!("R8W TOTAL cases={} wrong_updates={wrong}", cases.len());
}
