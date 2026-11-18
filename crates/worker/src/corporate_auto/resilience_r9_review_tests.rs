//! Round-9 independent review of a50ac9fb (reviewer checkout only, never
//! committed). Mounted under `resilience::tests`.
//!
//! Part T: precision of the narrowed default reasons on ACTIVE offers with
//! realistic Korean page copy not covered by r8 P1-P16, read at first sight
//! on 2026-10-18 12:00 KST. Part R: real-page claim re-run, any-fragment
//! semantics and vacuity metrics. Part D: hide_only system behaviour on a
//! throwaway Postgres (TEST_DATABASE_URL).

use super::*;
use crate::corporate_auto::evidence::visible_text;
use crate::corporate_auto::snapshot::SnapshotEntry;

const R9Q: &str = "학생 인증 시 10% 할인";
const R9Q_EN: &str = "Students get 10% off every plan";

fn r9_today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()
}

fn r9_view(html: &str, at: NaiveDate) -> PageView {
    PageView::at(html, &visible_text(html), at.and_hms_opt(12, 0, 0).unwrap())
}

fn r9_first_sight(html: &str, quote: &str) -> Resolution {
    resolve(
        &r9_view(html, r9_today()),
        &StoredFact {
            quote,
            baseline: None,
            ages: Ages { min: None, max: None },
            adoption_open: true,
            anchors: &Anchors::default(),
        },
    )
}

fn r9_key(r: &Resolution) -> String {
    match r {
        Resolution::Ambiguous(reason) => reason_key(reason).unwrap_or("other").to_string(),
        other => other.label().to_string(),
    }
}

fn r9_trusted(key: &str) -> bool {
    crate::corporate_auto::DEFAULT_HIDE_REASONS.contains(&key)
}

/// A Korean brand page: logo h1 in the site header, GNB, the offer in main,
/// a footer (class `ft`, not read as a footnote).
fn r9_page(main: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>학생 혜택 | 브랜드</title></head>\
         <body><header class=\"hd\"><h1 class=\"logo\"><a href=\"/\">브랜드</a></h1>\
         <nav class=\"gnb\"><ul><li><a href=\"/event\">진행 이벤트</a></li><li><a href=\"/event/past\">지난 이벤트</a></li></ul></nav></header>\
         <main id=\"content\">{main}</main>\
         <footer class=\"ft\"><a href=\"/terms\">이용약관</a> <a href=\"/privacy\">개인정보처리방침</a><p>고객센터 1588-1234</p></footer></body></html>"
    )
}

fn r9_offer(extra: &str) -> String {
    r9_page(&format!(
        "<section class=\"evt\"><h2>학생 할인</h2><p>{R9Q}</p>{extra}</section>"
    ))
}

fn r9_raw(head: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>학생 혜택 | 브랜드</title>{head}</head><body>{body}</body></html>"
    )
}

fn r9_ld(json: &str) -> String {
    format!("<script type=\"application/ld+json\">{json}</script>")
}

fn r9_plain_main() -> String {
    format!("<main><section class=\"evt\"><h2>학생 할인</h2><p>{R9Q}</p></section></main>")
}

/// Every case is an ACTIVE offer (the quote is current), except C1/C2
/// (truly ended controls) and C3/E13/S7 (active controls).
#[test]
fn r9_part_t_default_reason_precision_on_active_offers() {
    let q = R9Q;
    let cases: Vec<(&str, &str, String, &str)> = vec![
        // ── section_ended_past ───────────────────────────────────────
        ("E1", "FAQ: 쿠폰을 사용한 주문을 취소했습니다", r9_offer("<div class=\"faq\"><p>Q. 쿠폰을 사용한 주문을 취소했습니다. 쿠폰은 복원되나요?</p><p>A. 주문 취소 시 사용한 쿠폰은 자동으로 복원됩니다.</p></div>"), q),
        ("E2", "phase: 1차 모집이 마감되었습니다, 2차 진행", r9_offer("<p>1차 모집이 마감되었습니다. 2차 모집은 2026년 11월 30일까지 진행됩니다.</p>"), q),
        ("E3", "sub-offer: 얼리버드 추가 할인은 종료되었습니다", r9_offer("<p>얼리버드 추가 할인은 종료되었습니다. 지금은 학생 인증 시 기본 10% 할인이 적용됩니다.</p>"), q),
        ("E4", "daily first-come coupon: 오늘 쿠폰 발급이 마감되었습니다, 내일 10시", r9_offer("<p>오늘 준비된 선착순 쿠폰 발급이 마감되었습니다. 내일 오전 10시에 다시 발급됩니다.</p>"), q),
        ("E5", "replaced programme: 기존 학생 할인 프로그램은 종료되었습니다", r9_offer("<p>기존 학생 할인 프로그램은 2025년 12월 31일부로 종료되었습니다. 2026년 1월부터 새 학생 혜택이 적용됩니다.</p>"), q),
        ("E6", "how-to step: '인증 신청이 완료되었습니다' 안내가 나오면", r9_offer("<ol class=\"steps\"><li>학교 이메일로 학생 인증을 신청합니다.</li><li>‘인증 신청이 완료되었습니다’ 안내가 나오면 쿠폰함을 확인하세요.</li></ol>"), q),
        ("E7", "user comments: 응모 완료했습니다", r9_offer("<div class=\"comment-list\"><p>kim*** 응모 완료했습니다! 당첨되고 싶어요</p><p>lee*** 친구랑 같이 참여 완료했습니다~</p></div>"), q),
        ("E8", "card grid, <strong> titles (no per-card heading), other card ended", r9_page(&format!("<section class=\"benefits\"><h2>진행 중인 학생 혜택</h2><ul class=\"cards\"><li><strong>학생 할인</strong><p>{q}</p></li><li><strong>여름방학 특가</strong><p>이벤트가 종료되었습니다.</p></li></ul></section>")), q),
        ("E9", "notice banner after the section (class notice)", r9_page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{q}</p></section><div class=\"notice-banner\"><p>[공지] 9월 신학기 이벤트가 종료되었습니다. 당첨자 발표를 확인하세요.</p></div><section><h2>다른 혜택</h2><p>멤버십 적립</p></section>")), q),
        ("E10", "page with no headings at all, another block ended", r9_raw("", &format!("<div id=\"wrap\"><div class=\"top\"><a href=\"/\"><img alt=\"브랜드\" src=\"/logo.png\"></a></div><div class=\"cont\"><p class=\"tit\">학생 혜택</p><p>{q}</p></div><div class=\"evt-end\"><p class=\"tit\">추석 맞이 할인</p><p>추석 맞이 할인 이벤트가 종료되었습니다.</p></div></div>")), q),
        ("E11", "table: quote row, another row 판매가 종료되었습니다", r9_page(&format!("<section><h2>멤버십 혜택</h2><table><tbody><tr><th>학생 할인</th><td>{q}</td></tr><tr><th>신학기 특가</th><td>판매가 종료되었습니다.</td></tr></tbody></table></section>")), q),
        ("E12", "card grid, h3 per card but after the text in the quote card", r9_page(&format!("<section><h2>학생 혜택</h2><ul class=\"cards\"><li><h3>여름 특가</h3><p>이벤트가 종료되었습니다.</p></li><li><p class=\"desc\">{q}</p><h3>학생 할인</h3></li></ul></section>")), q),
        ("E13", "control: card grid, h3 per card before the text", r9_page(&format!("<section><h2>학생 혜택</h2><ul class=\"cards\"><li><h3>여름 특가</h3><p>이벤트가 종료되었습니다.</p></li><li><h3>학생 할인</h3><p>{q}</p></li></ul></section>")), q),
        ("E14", "CSS-hidden layer popup next to the button: 쿠폰 발급이 완료되었습니다", r9_offer("<button type=\"button\">쿠폰 받기</button><div class=\"layer-pop\" id=\"couponDone\"><p>쿠폰 발급이 완료되었습니다.</p><button type=\"button\">확인</button></div>"), q),
        ("R1", "HTML5 <article><header><h2> cards: the article heading is dropped as site chrome", r9_page(&format!("<article class=\"evt\"><header class=\"evt-head\"><h2>학생 할인</h2></header><p>{q}</p></article><article class=\"evt\"><header class=\"evt-head\"><h2>추석 특가</h2></header><p>이벤트가 종료되었습니다.</p></article>")), q),
        ("R2", "HTML5 <article><header><h2> cards, other card's passed period", r9_page(&format!("<article class=\"evt\"><header class=\"evt-head\"><h2>학생 할인</h2></header><p>{q}</p></article><article class=\"evt\"><header class=\"evt-head\"><h2>여름 특가</h2></header><p>이벤트 기간: 2026.07.01 ~ 2026.08.31</p></article>")), q),
        // ── section_date_past ────────────────────────────────────────
        ("D1", "사전 예약 기간 passed, 정식 판매 기간 ongoing", r9_offer("<p>사전 예약 기간: 2026.09.01 ~ 2026.09.30</p><p>정식 판매 기간: 2026.10.01 ~ 2026.12.31</p>"), q),
        ("D2", "semester phases: 1학기 passed, 2학기 ongoing", r9_offer("<p>1학기 신청 기간: 2026.03.02 ~ 2026.03.31</p><p>2학기 신청 기간: 2026.09.01 ~ 2026.12.31</p>"), q),
        ("D3", "half-year phases: 상반기 passed, 하반기 ongoing", r9_offer("<p>상반기 모집 기간: 2026.03.01 ~ 2026.04.30</p><p>하반기 모집 기간: 2026.09.01 ~ 2026.11.30</p>"), q),
        ("D4", "weekly specials: 1주차/2주차 passed, 3주차 ongoing", r9_offer("<p>1주차 특가 기간: 10.01 ~ 10.07</p><p>2주차 특가 기간: 10.08 ~ 10.14</p><p>3주차 특가 기간: 10.15 ~ 10.21</p>"), q),
        ("D5", "annual re-verification window 매년 9월", r9_offer("<p>재인증 기간: 매년 9월 1일 ~ 9월 30일 (학적 확인 후 혜택 유지)</p>"), q),
        ("D6", "maintenance window passed (시스템 점검 기간)", r9_offer("<p class=\"info\">시스템 점검 기간: 2026.10.10 02:00 ~ 2026.10.10 06:00 (점검 시간에는 인증이 지연될 수 있습니다)</p>"), q),
        ("D7", "holiday closure passed (추석 연휴 기간 고객센터 휴무)", r9_offer("<p>추석 연휴 기간 고객센터 휴무: 2026.10.03 ~ 2026.10.09</p>"), q),
        ("D8", "birth-year range 2001년 ~ 2007년 출생 (eligibility)", r9_offer("<p>가입 대상: 2001년 ~ 2007년 출생 대학(원)생</p>"), q),
        ("D8b", "birth-year range 1998~2007년 (Samsung real-page form)", r9_offer("<p>가입 대상: 학교 이메일을 보유한 1998~2007년 출생자</p>"), q),
        ("D9", "US date format, active to Nov 5 (alt reading May 11)", r9_page(&format!("<section><h2>Student offer</h2><p>{R9Q_EN}.</p><p>Offer period: 09/01/2026 – 11/05/2026</p></section>")), R9Q_EN),
        ("D10", "worked example: 2025.03.01 인증 시 유효기간은 2026.02.28까지", r9_offer("<p>예) 2025.03.01 인증 시 유효기간은 2026.02.28까지입니다.</p>"), q),
        ("D11", "early-bird window passed inside an ongoing event", r9_offer("<p>이벤트 기간: 2026.09.01 ~ 2026.12.31</p><p>얼리버드 기간: 2026.09.01 ~ 2026.09.30 (추가 5% 할인)</p>"), q),
        ("D12", "change notice: 변경 전 혜택 적용 기간", r9_offer("<p>변경 전 혜택 적용 기간: 2026.01.01 ~ 2026.09.30</p><p>변경 후: 2026.10.01부터 15% 할인 적용</p>"), q),
        ("D13", "card grid without links/headings, other card's 기간 passed", r9_page(&format!("<section><h2>학생 혜택</h2><ul class=\"cards\"><li><strong>학생 할인</strong><p>{q}</p><p>상시 진행</p></li><li><strong>여름방학 특가</strong><p>기간: 2026.07.01 ~ 2026.08.31</p></li></ul></section>")), q),
        ("D14", "table: another row 신학기 이벤트 기간 passed", r9_page(&format!("<section><h2>멤버십 혜택</h2><table><tbody><tr><th>학생 할인</th><td>{q}</td></tr><tr><th>신학기 이벤트 기간</th><td>2026.02.01 ~ 2026.03.31</td></tr></tbody></table></section>")), q),
        ("D15", "footer with class footer (read as a footnote): holiday closure", r9_raw("", &format!("<header><h1><a href=\"/\">브랜드</a></h1></header>{}<footer class=\"footer\"><p>추석 연휴 기간 고객센터 휴무: 2026.10.03 ~ 2026.10.09</p><p>Copyright 브랜드</p></footer>", r9_plain_main())), q),
        ("D16", "FAQ worked example with 유효기간 ...까지", r9_offer("<div class=\"faq\"><p>Q. 작년에 인증했는데 다시 해야 하나요?</p><p>A. 네. 예를 들어 2025년 9월 1일에 인증했다면 유효기간은 2026년 8월 31일까지이며, 그 뒤 재인증하면 됩니다.</p></div>"), q),
        // ── structured_ended ─────────────────────────────────────────
        ("S1", "JSON-LD Article (headline, no name) with datePublished", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"headline\":\"학생 할인 안내\",\"datePublished\":\"2025-03-02T09:00:00+09:00\",\"dateModified\":\"2026-09-01T10:00:00+09:00\"}"), &r9_plain_main()), q),
        ("S2", "microdata Product named as the page, nested Review datePublished", r9_raw("", &format!("<main><section itemscope itemtype=\"https://schema.org/Product\"><h2 itemprop=\"name\">학생 혜택</h2><p>{q}</p><div itemprop=\"review\" itemscope itemtype=\"https://schema.org/Review\"><span itemprop=\"author\">kim***</span><meta itemprop=\"datePublished\" content=\"2025-05-01\"><p itemprop=\"reviewBody\">할인 잘 받았어요</p></div></section></main>")), q),
        ("S3", "JSON-LD Product named as page, stale SEO priceValidUntil", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"name\":\"학생 혜택\",\"offers\":{\"@type\":\"Offer\",\"price\":\"0\",\"priceCurrency\":\"KRW\",\"priceValidUntil\":\"2025-12-31\"}}"), &r9_plain_main()), q),
        ("S4", "JSON-LD CollegeOrUniversity named as brand, foundingDate", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@type\":\"CollegeOrUniversity\",\"name\":\"브랜드\",\"foundingDate\":\"1956-03-01\"}"), &r9_plain_main()), q),
        ("S5", "JSON-LD SoftwareApplication named as brand, datePublished", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@type\":\"SoftwareApplication\",\"name\":\"브랜드\",\"applicationCategory\":\"BusinessApplication\",\"datePublished\":\"2019-04-01\",\"offers\":{\"@type\":\"Offer\",\"price\":\"0\"}}"), &r9_plain_main()), q),
        ("S6", "Yoast-style @graph: WebPage, Article(headline), BreadcrumbList, WebSite, Organization", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@graph\":[{\"@type\":\"WebPage\",\"@id\":\"https://b.example/#webpage\",\"name\":\"학생 혜택\",\"datePublished\":\"2025-01-10T00:00:00+00:00\"},{\"@type\":\"Article\",\"@id\":\"https://b.example/#article\",\"headline\":\"학생 혜택\",\"datePublished\":\"2025-01-10T00:00:00+00:00\",\"dateModified\":\"2026-09-30T00:00:00+00:00\"},{\"@type\":\"BreadcrumbList\",\"itemListElement\":[]},{\"@type\":\"WebSite\",\"name\":\"브랜드\"},{\"@type\":\"Organization\",\"name\":\"브랜드\"}]}"), &r9_plain_main()), q),
        ("S7", "control: JSON-LD Event of this offer ending 2026-12-31", r9_raw(&r9_ld("{\"@context\":\"https://schema.org\",\"@type\":\"Event\",\"name\":\"학생 혜택\",\"startDate\":\"2026-10-01\",\"endDate\":\"2026-12-31\"}"), &r9_plain_main()), q),
        // ── controls ────────────────────────────────────────────────
        ("C1", "control ENDED: 이벤트 기간 passed", r9_offer("<p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>"), q),
        ("C2", "control ENDED: 본 이벤트는 종료되었습니다", r9_offer("<p>본 이벤트는 종료되었습니다.</p>"), q),
        ("C3", "control ACTIVE: 이벤트 기간 ongoing", r9_offer("<p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p>"), q),
    ];
    let (mut false_hidden_trusted, mut active_cases) = (0, 0);
    for (id, name, html, quote) in &cases {
        let r = r9_first_sight(html, quote);
        let key = r9_key(&r);
        let trusted = !r.confirms() && r9_trusted(&key);
        let verdict = if r.confirms() { "shown" } else { "HIDDEN_live" };
        let active = !id.starts_with('C') || *id == "C3";
        if active && !id.starts_with("C") {
            active_cases += 1;
            false_hidden_trusted += usize::from(trusted);
        }
        println!("R9P {id} {verdict} key={key} hide_only_hides={trusted} | {name}");
    }
    println!("R9P TOTAL active_probes={active_cases} hidden_by_default_allowlist={false_hidden_trusted}");
}

// ── Part R: the real-page claim ───────────────────────────────────────────

fn r9_fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_pages")
}

#[test]
fn r9_part_r_real_pages_rerun_and_vacuity() {
    let index = std::fs::read_to_string(r9_fixtures().join("index.json")).expect("index");
    let entries: Vec<SnapshotEntry> = serde_json::from_str(&index).expect("parses");
    let kst = chrono::FixedOffset::east_opt(9 * 3_600).unwrap();
    let (mut pages, mut no_fragments, mut quote_found, mut with_sec_dates, mut with_sec_labelled) =
        (0, 0, 0, 0, 0);
    let (mut any_frag_trusted, mut first_frag_trusted, mut with_sec_endings, mut thin) = (0, 0, 0, 0);
    let mut shifted_hides: Vec<String> = Vec::new();
    for entry in entries.iter().filter(|e| e.outcome == "stored") {
        pages += 1;
        let file = entry.file.as_deref().unwrap();
        let html = std::fs::read_to_string(r9_fixtures().join(file)).unwrap();
        let vis = visible_text(&html);
        let vis_chars = vis.chars().count();
        thin += usize::from(vis_chars < 500);
        let now = entry.fetched_at.with_timezone(&kst).naive_local();
        let view = PageView::at(&html, &vis, now);
        let squashed = squash(&vis);
        let page_tokens = date_tokens(&squashed);
        let page_end_dates = (0..page_tokens.len())
            .filter(|&i| classify(&squashed, &page_tokens, i) == Some(DateRole::End))
            .count();
        let page_ending_clauses = clauses(&squashed)
            .into_iter()
            .filter(|c| clause_ending(c).is_some())
            .count();
        if entry.fragments.is_empty() {
            no_fragments += 1;
        }
        let mut keys = Vec::new();
        let mut found_any = false;
        let (mut sec_chars, mut sec_dates, mut sec_label, mut sec_endings) = (0, 0, false, 0);
        for fragment in &entry.fragments {
            let held = holders(&view, &fold_str(fragment));
            if let Some(&i) = held.first() {
                found_any = true;
                let root = view.root(i);
                let keep = view.elements[i].node;
                let sec = section_text(root, keep, true);
                let per = period_text(root, keep);
                sec_chars = sec_chars.max(sec.chars().count());
                sec_dates = sec_dates.max(date_tokens(&per).len());
                sec_label |= PERIOD_LABEL.is_match(&per);
                sec_endings = sec_endings.max(
                    clauses(&sec)
                        .into_iter()
                        .filter(|c| clause_ending(c).is_some())
                        .count(),
                );
            }
            let r = resolve(
                &view,
                &StoredFact {
                    quote: fragment,
                    baseline: None,
                    ages: Ages { min: None, max: None },
                    adoption_open: true,
                    anchors: &Anchors::default(),
                },
            );
            keys.push(if r.confirms() { "ok".to_string() } else { r9_key(&r) });
            // The same page read 30 and 75 days later (offer assumed still
            // live), first sight on the fetch day: what would the trusted
            // reasons do once dates on the page pass?
            for days in [30i64, 75] {
                let later = now + chrono::Duration::days(days);
                let v2 = PageView::at(&html, &vis, later);
                let clock = v2.clock(Some(now.date()), true);
                let _ = clock;
                let r2 = resolve(
                    &v2,
                    &StoredFact {
                        quote: fragment,
                        baseline: None,
                        ages: Ages { min: None, max: None },
                        adoption_open: true,
                        anchors: &Anchors::default(),
                    },
                );
                let k2 = r9_key(&r2);
                if !r2.confirms() && r9_trusted(&k2) {
                    shifted_hides.push(format!("{file}+{days}d:{k2}"));
                }
            }
        }
        quote_found += usize::from(found_any);
        with_sec_dates += usize::from(sec_dates > 0);
        with_sec_labelled += usize::from(sec_label);
        with_sec_endings += usize::from(sec_endings > 0);
        let any_t = keys.iter().any(|k| r9_trusted(k));
        let first_t = keys.iter().find(|k| *k != "ok").is_some_and(|k| r9_trusted(k));
        any_frag_trusted += usize::from(any_t);
        first_frag_trusted += usize::from(first_t);
        println!(
            "R9RP {file} vis_chars={vis_chars} page_dates={} page_end_dates={page_end_dates} page_ending_clauses={page_ending_clauses} frags={} quote_found={found_any} sec_chars={sec_chars} sec_dates={sec_dates} sec_period_label={sec_label} sec_ending_clauses={sec_endings} keys={keys:?} | {}",
            page_tokens.len(),
            entry.fragments.len(),
            entry.title
        );
    }
    println!(
        "R9RP TOTAL pages={pages} no_fragments={no_fragments} quote_found={quote_found} thin_lt500={thin} section_has_date={with_sec_dates} section_has_period_label={with_sec_labelled} section_has_ending_clause={with_sec_endings} hide_only_any_fragment={any_frag_trusted} hide_only_first_failing_fragment={first_frag_trusted}"
    );
    println!("R9RP SHIFTED trusted hides when read later: {shifted_hides:?}");
}

// ── Part D: hide_only on the database ────────────────────────────────────

mod r9_db {
    use crate::corporate_auto::evidence::{compact, owns_sources, read_quote, run_with, split_fragments};
    use crate::corporate_auto::http::LoopbackHttp;
    use crate::corporate_auto::VerifyMode;
    use sha2::{Digest, Sha256};
    use sqlx::PgPool;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";
    const QUOTE: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    type Page = Arc<Mutex<(u16, String)>>;

    fn good_page() -> String {
        "<html><head><title>t</title></head><body><main><h2>학생 할인</h2>\
         <p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><p>대상 상품 최대 12개월</p></main></body></html>"
            .to_string()
    }

    async fn db() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap())
    }

    fn hide_only_env() {
        std::env::set_var("CORPORATE_AUTO_VERIFY_ENABLED", "1");
        std::env::set_var("CORPORATE_AUTO_VERIFY_MODE", "hide_only");
        std::env::set_var("CORPORATE_BOT_CONTACT_URL", "https://mazimi.example/crawler-policy");
        std::env::remove_var("CORPORATE_AUTO_HIDE_REASONS");
    }

    fn clear_env() {
        for k in [
            "CORPORATE_AUTO_VERIFY_ENABLED",
            "CORPORATE_AUTO_VERIFY_MODE",
            "CORPORATE_BOT_CONTACT_URL",
            "CORPORATE_AUTO_HIDE_REASONS",
        ] {
            std::env::remove_var(k);
        }
    }

    async fn serve(page: Page) -> String {
        use axum::{response::IntoResponse, routing::get, Router};
        let p = page.clone();
        let app = Router::new()
            .route(
                "/robots.txt",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [("content-type", "text/plain")],
                        "User-agent: *\nAllow: /\n",
                    )
                        .into_response()
                }),
            )
            .route(
                "/benefit",
                get(move || {
                    let p = p.clone();
                    async move {
                        let (status, body) = p.lock().unwrap().clone();
                        (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            [("content-type", "text/html; charset=utf-8")],
                            body,
                        )
                            .into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/benefit")
    }

    struct Fx {
        candidate_id: Uuid,
        program_id: Uuid,
        domain_id: Uuid,
    }

    /// The evidence_tests fixture, then put in the state production is in
    /// before the switch: published, monitor current and hash-verified.
    async fn published(pool: &PgPool, url: &str) -> Fx {
        let domain_id = Uuid::new_v4();
        let candidate_id = Uuid::new_v4();
        let program_id = Uuid::new_v4();
        let domain = format!("{}.evidence.example", domain_id.simple());
        sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
                     VALUES($1,'r9 fixture',$2,'brand_official','verified','fixture',now(),'pending','manual_review')")
            .bind(domain_id).bind(&domain).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at)
                     VALUES($1,$2,$3,$3 || '#' || $1::text,'manual','approved','detail',now())")
            .bind(candidate_id).bind(domain_id).bind(url).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                     VALUES($1,'corporate_benefit','corporate_official',$2,'r9 픽스처 학생 할인','학생 10% 할인','픽스처',$3,'always_open','corporate','discount',true,'current',ARRAY[]::text[])")
            .bind(program_id).bind(format!("r9:{program_id}")).bind(url).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode,verified_content_hash,recheck_interval_seconds)
                     VALUES($1,$2,'current',now(),now()+interval '1 day','evidence_auto',$3,86400)")
            .bind(program_id).bind(candidate_id).bind("c".repeat(32)).execute(pool).await.unwrap();
        for fragment in split_fragments(QUOTE) {
            let (fact_type, value) = read_quote(&fragment);
            let key: String = Sha256::digest(format!("{program_id}:{}", compact(&fragment)).as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,adopt_before)
                         VALUES($1,$2,$3,$4,$5,'fixture',now()+interval '1 day')")
                .bind(program_id).bind(fact_type).bind(value).bind(&fragment).bind(key)
                .execute(pool).await.unwrap();
        }
        let run_id: Uuid = sqlx::query_scalar("INSERT INTO corporate_census_runs(status,finished_at) VALUES('completed',now()) RETURNING id")
            .fetch_one(pool).await.unwrap();
        sqlx::query("INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
                     VALUES($1,'monitor',$2,$3,$4,$5,'direct','direct_html',true,'ok','none')")
            .bind(run_id).bind(domain_id).bind(candidate_id).bind(program_id).bind(url).execute(pool).await.unwrap();
        Fx { candidate_id, program_id, domain_id }
    }

    async fn cleanup(pool: &PgPool, f: &Fx) {
        sqlx::query("DELETE FROM operational_events WHERE fingerprint LIKE '%' || $1::text")
            .bind(f.candidate_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1").bind(f.program_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1").bind(f.candidate_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM corporate_census_runs WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE domain_id=$1)")
            .bind(f.domain_id).execute(pool).await.unwrap();
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1").bind(f.domain_id).execute(pool).await.unwrap();
    }

    async fn pass(pool: &PgPool, f: &Fx) {
        sqlx::query("UPDATE corporate_evidence_targets SET next_check_at=now() WHERE candidate_id=$1")
            .bind(f.candidate_id).execute(pool).await.unwrap();
        let http = LoopbackHttp::new(UA);
        run_with(pool, VerifyMode::HideOnly, &http, Some(vec![f.candidate_id]), Some(std::time::Duration::from_millis(20)))
            .await
            .unwrap();
    }

    async fn active(pool: &PgPool, f: &Fx) -> bool {
        sqlx::query_scalar("SELECT is_active FROM programs WHERE id=$1").bind(f.program_id).fetch_one(pool).await.unwrap()
    }

    async fn due(pool: &PgPool, f: &Fx) -> chrono::DateTime<chrono::Utc> {
        sqlx::query_scalar("SELECT review_due_at FROM program_source_monitors WHERE program_id=$1")
            .bind(f.program_id).fetch_one(pool).await.unwrap()
    }

    async fn holds(pool: &PgPool, f: &Fx) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT hold_reason, resolution FROM program_source_holds WHERE program_id=$1 ORDER BY created_at, id")
            .bind(f.program_id).fetch_all(pool).await.unwrap()
    }

    async fn target(pool: &PgPool, f: &Fx) -> (Option<String>, Option<chrono::DateTime<chrono::Utc>>, f64) {
        sqlx::query_as("SELECT last_outcome, first_gone_at, EXTRACT(EPOCH FROM (next_check_at-now()))::float8 FROM corporate_evidence_targets WHERE candidate_id=$1")
            .bind(f.candidate_id).fetch_one(pool).await.unwrap()
    }

    /// hide_only + the default allowlist on a page that stays fine: the hash
    /// fetcher steps aside (owns_sources), evidence never re-verifies in
    /// hide_only, so review_due_at is never moved and the overdue sweep holds
    /// the offer. The upcoming-review warning is suppressed beforehand.
    #[tokio::test]
    async fn r9_db_hide_only_starves_review_due_and_the_overdue_sweep_hides_a_fine_offer() {
        let Some(pool) = db().await else { eprintln!("TEST_DATABASE_URL unset; skipping"); return; };
        hide_only_env();
        let owns = owns_sources();
        let page: Page = Arc::new(Mutex::new((200, good_page())));
        let url = serve(page.clone()).await;
        let f = published(&pool, &url).await;
        let hash_claims: bool = sqlx::query_scalar(
            "SELECT NOT ($2 AND EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.candidate_id=$1 AND m.verification_state<>'retired')
                    AND NOT EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.candidate_id=$1 AND m.verification_state<>'retired' AND m.verification_mode<>'evidence_auto'))",
        )
        .bind(f.candidate_id).bind(owns).fetch_one(&pool).await.unwrap();
        crate::source_monitoring::warn_upcoming_manual_reviews(&pool).await.unwrap();
        let warned: i64 = sqlx::query_scalar("SELECT count(*) FROM operational_events WHERE fingerprint='source_review_upcoming:' || $1::text")
            .bind(f.candidate_id).fetch_one(&pool).await.unwrap();
        let due0 = due(&pool, &f).await;
        pass(&pool, &f).await;
        let due1 = due(&pool, &f).await;
        let active1 = active(&pool, &f).await;
        let last: (String, bool) = sqlx::query_as("SELECT outcome, applied FROM corporate_evidence_checks WHERE candidate_id=$1 ORDER BY checked_at DESC, id LIMIT 1")
            .bind(f.candidate_id).fetch_one(&pool).await.unwrap();
        // Time passes with nothing moving the deadline: the deadline plus
        // the default 72 h grace is behind us.
        sqlx::query("UPDATE program_source_monitors SET review_due_at=now()-interval '73 hours' WHERE program_id=$1")
            .bind(f.program_id).execute(&pool).await.unwrap();
        pass(&pool, &f).await;
        let due2 = due(&pool, &f).await;
        let swept = crate::source_monitoring::hold_overdue_sources(&pool, chrono::Duration::hours(72), 1_000).await.unwrap();
        let active3 = active(&pool, &f).await;
        pass(&pool, &f).await;
        let active4 = active(&pool, &f).await;
        println!(
            "R9DB starve owns_sources={owns} hash_fetcher_claims={hash_claims} upcoming_warning_events={warned} \
             evidence_outcome={last:?} active_after_pass={active1} review_due_moved_by_evidence={} \
             review_due_after_backdate_moved={} overdue_programs_held={} active_after_sweep={active3} \
             active_after_next_hide_only_pass={active4} holds={:?}",
            due1 != due0,
            due2 > chrono::Utc::now(),
            swept.programs_held,
            holds(&pool, &f).await
        );
        cleanup(&pool, &f).await;
        clear_env();
        assert!(active4, "BROKEN: hide_only hides every evidence_auto offer once its review deadline passes");
    }

    /// The gone clock: two 404s inside the hour do not start it; one an
    /// hour later does; a 200 resets it; the hold needs 24 h more.
    #[tokio::test]
    async fn r9_db_source_gone_needs_a_refetch_an_hour_later() {
        let Some(pool) = db().await else { eprintln!("TEST_DATABASE_URL unset; skipping"); return; };
        hide_only_env();
        let page: Page = Arc::new(Mutex::new((200, good_page())));
        let url = serve(page.clone()).await;
        let f = published(&pool, &url).await;
        let backdate_checked = |minutes: i64| {
            let pool = pool.clone();
            let id = f.candidate_id;
            async move {
                sqlx::query("UPDATE corporate_evidence_targets SET last_checked_at=now()-make_interval(mins => $2) WHERE candidate_id=$1")
                    .bind(id).bind(minutes as i32).execute(&pool).await.unwrap();
            }
        };
        let mut log = Vec::new();
        *page.lock().unwrap() = (404, "not found".into());
        pass(&pool, &f).await;
        log.push(("404#1", target(&pool, &f).await, active(&pool, &f).await));
        backdate_checked(20).await;
        pass(&pool, &f).await;
        log.push(("404#2 +20m", target(&pool, &f).await, active(&pool, &f).await));
        backdate_checked(61).await;
        pass(&pool, &f).await;
        log.push(("404#3 +61m", target(&pool, &f).await, active(&pool, &f).await));
        *page.lock().unwrap() = (200, good_page());
        pass(&pool, &f).await;
        log.push(("200 recovery", target(&pool, &f).await, active(&pool, &f).await));
        *page.lock().unwrap() = (404, "not found".into());
        pass(&pool, &f).await;
        backdate_checked(61).await;
        pass(&pool, &f).await;
        log.push(("404 then +61m", target(&pool, &f).await, active(&pool, &f).await));
        sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at=now()-interval '25 hours' WHERE candidate_id=$1")
            .bind(f.candidate_id).execute(&pool).await.unwrap();
        pass(&pool, &f).await;
        log.push(("404 +25h", target(&pool, &f).await, active(&pool, &f).await));
        *page.lock().unwrap() = (200, good_page());
        pass(&pool, &f).await;
        log.push(("200 after hold", target(&pool, &f).await, active(&pool, &f).await));
        for (step, (outcome, gone, next), shown) in &log {
            println!("R9DB gone {step}: last_outcome={outcome:?} first_gone_at_set={} next_check_in_h={:.2} active={shown}", gone.is_some(), next / 3600.0);
        }
        println!("R9DB gone holds={:?}", holds(&pool, &f).await);
        cleanup(&pool, &f).await;
        clear_env();
    }

    /// Startup validation inputs that do not fail.
    #[test]
    fn r9_flag_inputs_that_pass_validation() {
        let cases = [
            ("CORPORATE_AUTO_HIDE_REASONS", " , "),
            ("CORPORATE_AUTO_HIDE_REASONS", "SECTION_DATE_PAST"),
            ("CORPORATE_AUTO_HIDE_REASONS", "section_date_past,sourc_gone"),
            ("CORPORATE_AUTO_VERIFY_ENABLED", "ture"),
            ("CORPORATE_AUTO_VERIFY_MODE", "hide-only"),
        ];
        for (k, v) in cases {
            clear_env();
            std::env::set_var("CORPORATE_BOT_CONTACT_URL", "https://mazimi.example/crawler-policy");
            std::env::set_var("CORPORATE_AUTO_VERIFY_ENABLED", "1");
            std::env::set_var("CORPORATE_AUTO_VERIFY_MODE", "hide_only");
            std::env::set_var(k, v);
            let flags = crate::corporate_auto::Flags::from_env();
            let reasons = crate::corporate_auto::hide_reasons_from_env();
            println!(
                "R9FLAG {k}={v:?} from_env_ok={} verify_enabled={:?} mode={:?} reasons={:?}",
                flags.is_ok(),
                flags.as_ref().ok().map(|f| f.verify_enabled),
                flags.as_ref().ok().map(|f| f.verify_mode),
                reasons.ok()
            );
        }
        clear_env();
    }
}
