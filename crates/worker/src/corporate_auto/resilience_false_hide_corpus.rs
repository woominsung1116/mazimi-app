//! False-hide corpus: realistic, active Korean event and benefit pages (the
//! offer runs until 2026-12-31) and the round-7 reviewer's 14 false-hide
//! probes. Every page should confirm at first sight; each hide is a false
//! hide, counted by reason. The counts are a ceiling: a change that hides
//! more of these pages fails here.

use std::collections::BTreeMap;

use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::resilience::{
    reason_key, resolve, Anchors, PageView, Resolution, StoredFact,
};

/// The day the corpus is read (`today` is set on the view).
fn today() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()
}

fn shell(brand: &str, main: &str, aside: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>{brand} 이벤트</title>\
         <script type=\"application/ld+json\">{{\"@type\":\"Organization\",\"name\":\"{brand}\"}}</script></head>\
         <body><div class=\"skip\"><a href=\"#content\">본문 바로가기</a></div>\
         <header class=\"header\"><h1 class=\"logo\"><a href=\"/\">{brand}</a></h1>\
         <ul class=\"gnb\"><li><a href=\"/event\">진행중 이벤트</a></li><li><a href=\"/event/end\">지난 이벤트</a></li>\
         <li><a href=\"/notice\">공지사항</a></li><li><a href=\"/faq\">자주 묻는 질문</a></li></ul></header>\
         <div class=\"location\"><a href=\"/\">홈</a> &gt; <a href=\"/event\">이벤트</a></div>\
         <main id=\"content\">{main}</main>{aside}\
         <footer class=\"footer\"><ul class=\"policy\"><li><a href=\"/terms\">이용약관</a></li><li><a href=\"/privacy\">개인정보처리방침</a></li></ul>\
         <p>(주){brand} 대표이사 홍길동 · 사업자등록번호 123-45-67890 · 고객센터 1588-1234 (평일 09:00~18:00)</p>\
         <p>Copyright © 2026 {brand}. All rights reserved.</p></footer></body></html>"
    )
}

fn past_events_aside() -> String {
    "<aside class=\"side\"><h3>지난 이벤트</h3><ul>\
     <li><a href=\"/e/101\">여름 휴가 이벤트</a> <span>2026.07.01 ~ 2026.08.31</span></li>\
     <li><a href=\"/e/100\">봄맞이 신학기 이벤트</a> <span>2026.03.01 ~ 2026.03.31 종료</span></li></ul></aside>"
        .to_string()
}

/// Terms most active event pages carry, none of them an ended state.
const TERMS: &[&str] = &[
    "※ 본 이벤트는 당사 사정에 따라 사전 고지 없이 변경 또는 조기 종료될 수 있습니다.",
    "※ 재고 소진 시 조기 종료될 수 있습니다.",
    "※ 쿠폰 유효기간이 만료되면 자동 소멸됩니다.",
    "※ 이벤트 경품은 2026년 12월 15일 이후 순차 발송됩니다.",
    "※ 당첨자 발표: 2026년 12월 20일 (개별 연락)",
    "※ 취소 및 환불은 결제일로부터 7일 이내 마이페이지에서 가능합니다.",
    "※ 부정한 방법으로 참여한 경우 당첨이 취소될 수 있습니다.",
    "※ 제세공과금은 당사가 부담합니다.",
    // Exclusions most terms also carry: ordinary commerce wording that does
    // not name the offer, so it never hides it.
    "※ 다른 쿠폰과 중복 사용은 불가합니다.",
    "※ 일부 품목은 할인 대상에서 제외됩니다.",
];

struct Offer {
    brand: &'static str,
    title: &'static str,
    quote: &'static str,
    detail: &'static str,
}

const OFFERS: &[Offer] = &[
    Offer {
        brand: "카페온",
        title: "대학생 음료 할인",
        quote: "학생증 제시 시 전 음료 20% 할인",
        detail: "1일 1회, 1인 최대 2잔까지 적용됩니다.",
    },
    Offer {
        brand: "북스몰",
        title: "신학기 도서 할인",
        quote: "대학생 인증 시 전공 도서 10% 할인",
        detail: "온라인 주문에 한해 무료 배송을 제공합니다.",
    },
    Offer {
        brand: "모바일텔",
        title: "청년 요금제 혜택",
        quote: "만 34세 이하 고객 월 5,000원 할인",
        detail: "가입 후 24개월 동안 매월 자동 할인됩니다.",
    },
    Offer {
        brand: "무비플렉스",
        title: "청춘 영화 관람 혜택",
        quote: "대학생 인증 시 2D 영화 관람권 3,000원 할인",
        detail: "주말 및 공휴일에도 동일하게 적용됩니다.",
    },
    Offer {
        brand: "에듀랩",
        title: "어학 강의 지원",
        quote: "재학생 인증 시 수강료 30% 할인",
        detail: "강의 수강 기간은 결제일로부터 90일입니다.",
    },
    Offer {
        brand: "핏짐",
        title: "청년 헬스 멤버십",
        quote: "대학생 6개월 회원권 15% 할인",
        detail: "운동복과 수건을 무료로 제공합니다.",
    },
    Offer {
        brand: "트래블고",
        title: "청년 여행 지원",
        quote: "만 19세~29세 고객 국내 숙소 1만 원 할인 쿠폰 제공",
        detail: "쿠폰은 발급일로부터 30일간 사용할 수 있습니다.",
    },
    Offer {
        brand: "스트림뮤직",
        title: "학생 음원 요금제",
        quote: "대학생 인증 시 첫 3개월 월 4,900원",
        detail: "이후 정상가로 자동 결제되며 언제든 해지할 수 있습니다.",
    },
];

type Layout = fn(&Offer, &str) -> String;

fn plain(o: &Offer, terms: &str) -> String {
    format!(
        "<section class=\"event\"><h2>{}</h2><p class=\"period\">이벤트 기간: 2026.10.01 ~ 2026.12.31</p>\
         <p class=\"offer\">{}</p><p>{}</p><div class=\"btn-area\"><a href=\"/apply\" class=\"btn\">지금 신청하기</a></div>\
         <div class=\"notice\"><h3>유의사항</h3><ul>{terms}</ul></div></section>",
        o.title, o.quote, o.detail
    )
}

fn info_table(o: &Offer, terms: &str) -> String {
    format!(
        "<section><h2>{}</h2><table class=\"info\"><tr><th>대상</th><td>대학생 및 청년</td></tr>\
         <tr><th>혜택</th><td>{}</td></tr><tr><th>이벤트 기간</th><td>2026.10.01 ~ 2026.12.31</td></tr>\
         <tr><th>신청 마감일</th><td>2026.12.31</td></tr></table><p>{}</p>\
         <div class=\"caution\"><strong>꼭 확인하세요</strong><ul>{terms}</ul></div></section>",
        o.title, o.quote, o.detail
    )
}

fn definition_list(o: &Offer, terms: &str) -> String {
    format!(
        "<article class=\"view\"><h2 class=\"tit\">{}</h2><dl class=\"meta\"><dt>등록일</dt><dd>2026.10.01</dd><dt>조회</dt><dd>1,234</dd></dl>\
         <dl class=\"summary\"><dt>혜택 내용</dt><dd>{}</dd><dt>마감일</dt><dd>2026년 12월 31일 (목) 23:59까지</dd></dl>\
         <p>{}</p><ul class=\"terms\">{terms}</ul><div class=\"share\"><button type=\"button\">공유하기</button></div></article>",
        o.title, o.quote, o.detail
    )
}

fn tabbed(o: &Offer, terms: &str) -> String {
    format!(
        "<div class=\"tabs\" role=\"tablist\"><button role=\"tab\" aria-selected=\"true\" class=\"on\">진행중</button>\
         <button role=\"tab\">종료된 이벤트</button></div>\
         <div class=\"tab-pane active\" role=\"tabpanel\"><section><h2>{}</h2><span class=\"badge\">마감 D-74</span>\
         <p class=\"offer\">{}</p><p>{}</p><p class=\"remain\">신청 마감까지 74일 남았어요</p><ul>{terms}</ul></section></div>",
        o.title, o.quote, o.detail
    )
}

fn cards(o: &Offer, terms: &str) -> String {
    format!(
        "<section class=\"benefit\"><h2>{}</h2><div class=\"cards\"><div class=\"card\"><div class=\"card-head\"><strong>{}</strong></div>\
         <div class=\"card-body\"><p>{}</p><p>{}</p></div></div></div>\
         <div class=\"guide\"><h3>참여 방법</h3><ol><li>앱 로그인</li><li>학생 인증</li><li>쿠폰 다운로드</li></ol></div>\
         <div class=\"notice\"><h3>유의사항</h3><ul>{terms}</ul></div>\
         <p class=\"comment\">댓글 12</p></section>",
        o.title, o.brand, o.quote, o.detail
    )
}

const LAYOUTS: &[(&str, Layout)] = &[
    ("plain", plain),
    ("info_table", info_table),
    ("definition_list", definition_list),
    ("tabbed", tabbed),
    ("cards", cards),
];

fn view(html: &str) -> PageView {
    let mut v = PageView::new(html, &visible_text(html));
    v.today = today();
    v
}

fn adopted(html: &str, quote: &str) -> Resolution {
    // The programme stores the ages its quote states.
    let ages = crate::ai_extraction::crosscheck::parse_age(quote).map_or(
        Ages {
            min: None,
            max: None,
        },
        |r| Ages {
            min: r.min,
            max: r.max,
        },
    );
    resolve(
        &view(html),
        &StoredFact {
            quote,
            baseline: None,
            ages,
            adoption_open: true,
            anchors: &Anchors::default(),
        },
    )
}

/// The round-7 reviewer's false-hide probes (E7, E9, H1–H12), rebuilt with
/// the same text around the same offer.
/// Each probe is the main content and what sits after it outside main.
fn probes() -> Vec<(&'static str, String, String)> {
    let sec = |extra: &str| {
        format!("<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 10% 할인</p>{extra}</section>")
    };
    let note = |text: &str| sec(&format!("<p class=\"note\">{text}</p>"));
    let main = |m: String| (m, String::new());
    let entries: Vec<(&'static str, (String, String))> = vec![
        ("E7", main(note("1차 2026.09.01~09.30 / 2차 2026.10.01~2026.12.31"))),
        ("E9", main(note("경품은 2026년 11월 10일 이후 순차 발송됩니다"))),
        ("H1", main(sec("<dl class=\"info\"><dt>마감일</dt><dd>2026.12.31</dd></dl>"))),
        ("H2", main(sec("<table><tr><th>이벤트 종료일</th><td>2026.12.31</td></tr></table>"))),
        ("H3", main(sec("<h3>취소 및 환불 안내</h3><p>결제일로부터 7일 이내 요청하실 수 있습니다.</p>"))),
        ("H4", main(note("결제 후 주문 취소는 마이페이지에서 가능합니다"))),
        ("H5", main(sec("<span class=\"badge\">마감 D-5</span>"))),
        ("H6", (sec(""), "<div class=\"mail\"><p>더 이상 메일 수신을 원하지 않으시면 수신거부를 눌러주세요</p></div>".into())),
        ("H7", main(sec("<h3>포인트 유효기간 만료 안내</h3><p>적립일로부터 1년간 사용 가능합니다.</p>"))),
        ("H8", main(format!("{}<aside><h3>지난 소식</h3><p>추석 이벤트 ~2026.09.30</p></aside>", sec("")))),
        ("H9", main(format!("<div class=\"top-notice\"><p>2026년 11월 1일부터 개정 약관이 적용됩니다.</p></div>{}", sec("")))),
        ("H10", main(sec("<h3>자동결제 해지·중단 방법</h3><p>마이페이지에서 설정할 수 있습니다.</p>"))),
        ("H11", main(sec("<p class=\"remain\">신청 마감까지 3일 남았어요</p>"))),
        ("H12", main(sec("<table><tr><th>판매 종료</th><td>상시 판매</td></tr></table>"))),
    ];
    entries
        .into_iter()
        .map(|(id, (m, after))| (id, m, after))
        .collect()
}

#[test]
fn realistic_active_pages_are_counted_for_false_hides() {
    let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut pages = 0usize;
    let mut record = |id: String, resolution: Resolution| {
        if resolution.confirms() {
            return;
        }
        let key = match &resolution {
            Resolution::Ambiguous(reason) => reason_key(reason).unwrap_or("other").to_string(),
            Resolution::Exact(FactOutcome::QuoteMissing) => "quote_missing".to_string(),
            other => other.label().to_string(),
        };
        by_reason.entry(key).or_default().push(id);
    };
    for (index, offer) in OFFERS.iter().enumerate() {
        for (name, layout) in LAYOUTS {
            // Each page carries four of the usual terms, rotated.
            let terms: String = (0..4)
                .map(|k| format!("<li>{}</li>", TERMS[(index + k) % TERMS.len()]))
                .collect();
            let aside = if index % 2 == 0 {
                past_events_aside()
            } else {
                String::new()
            };
            let html = shell(offer.brand, &layout(offer, &terms), &aside);
            pages += 1;
            record(
                format!("{}:{name}", offer.brand),
                adopted(&html, offer.quote),
            );
        }
    }
    let probes = probes();
    for (id, main, after) in &probes {
        let html = shell("브랜드", main, after);
        record((*id).to_string(), adopted(&html, "학생 인증 시 10% 할인"));
    }
    let hidden: usize = by_reason.values().map(Vec::len).sum();
    eprintln!(
        "false-hide corpus: {} realistic pages + {} probes, {hidden} hidden: {by_reason:?}",
        pages,
        probes.len()
    );
    assert!(pages >= 40);
    // Ceiling on the known false hides: E7 (one phase of a two-phase event
    // has ended) and H12 (a 판매 종료 label without a date). Ordinary
    // commerce exclusions (`중복 사용은 불가`, `일부 품목 제외`, `에 한해`)
    // never hide (handoff §17).
    assert!(hidden <= 2, "{by_reason:#?}");
    assert!(
        !by_reason.contains_key("section_excluded"),
        "{by_reason:#?}"
    );
}
