//! Round-5 independent adversarial replay (reviewer-written, shares no code
//! with the implementation or its other tests). Only the public API is used:
//! `PageView::new`, `resolve`, `capture`, `StoredFact`, `Anchors`,
//! `Resolution` and `evidence::{visible_text, Ages, FactOutcome}`.
//!
//! Production order is replayed for every case: seed (no baseline, no
//! anchors, adoption open) must be an adopted exact confirmation; the fact
//! is then confirmed once more on the same page with the captured anchors
//! (anchors recaptured, as `assess` does); then the after-page is resolved
//! with the live baseline and the stored anchors.

use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::resilience::{
    capture, resolve, Anchors, PageView, Resolution, StoredFact,
};

const NO_AGES: Ages = Ages {
    min: None,
    max: None,
};

fn page(body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>혜택 안내</title>\
         <script>window.__STATE__={{\"rate\":\"10%\"}}</script><style>.x{{color:red}}</style></head>\
         <body><header class=\"gnb\"><a href=\"/\">홈</a> <a href=\"/benefit\">혜택</a></header>\
         <main id=\"content\">{body}</main><footer class=\"ft\"><p>고객센터 1588-1234</p></footer></body></html>"
    )
}

fn view(html: &str) -> PageView {
    PageView::new(html, &visible_text(html))
}

/// No values, no ending words; about 55 folded chars a copy.
const FILLER: &str = "본 혜택은 제휴사 사정에 따라 변경될 수 있으며 자세한 내용은 고객센터로 문의해 주시기 바랍니다.";

fn filler(times: usize) -> String {
    (0..times)
        .map(|_| FILLER)
        .collect::<Vec<_>>()
        .join(" ")
}

struct Stored {
    quote: String,
    baseline: String,
    anchors: Anchors,
}

fn fact<'a>(quote: &'a str, baseline: Option<&'a str>, anchors: &'a Anchors, open: bool) -> StoredFact<'a> {
    StoredFact {
        quote,
        baseline,
        ages: NO_AGES,
        adoption_open: open,
        anchors,
    }
}

/// Seed + confirm on the same page, as production does.
fn seed(html: &str, quote: &str) -> Result<Stored, String> {
    let v = view(html);
    let empty = Anchors::default();
    let first = resolve(&v, &fact(quote, None, &empty, true));
    let window = match &first {
        Resolution::Exact(FactOutcome::Confirmed {
            window_signature,
            adopted: true,
        }) => window_signature.clone(),
        other => return Err(format!("seed not adopted: {other:?}")),
    };
    let anchors = Anchors {
        fingerprint: capture(&v, quote),
        ..Anchors::default()
    };
    if anchors.fingerprint.is_none() {
        return Err("capture found no holder".into());
    }
    let second = resolve(&v, &fact(quote, Some(&window), &anchors, false));
    if !matches!(second, Resolution::Exact(FactOutcome::Confirmed { .. })) {
        return Err(format!("second confirm on the seed page failed: {second:?}"));
    }
    let anchors = Anchors {
        fingerprint: capture(&v, quote),
        ..anchors
    };
    Ok(Stored {
        quote: quote.to_string(),
        baseline: window,
        anchors,
    })
}

fn after(stored: &Stored, html: &str) -> Resolution {
    let v = view(html);
    resolve(
        &v,
        &fact(&stored.quote, Some(&stored.baseline), &stored.anchors, false),
    )
}

#[derive(Clone, Copy, Debug)]
enum Expect {
    /// The offer is unchanged: confirm (a hide is a false hide).
    Same,
    /// The offer is gone, changed in a way no value proposal can carry, or
    /// now carries a condition: never confirm, never propose.
    Gone,
    /// The value changed to this signature: a proposal with exactly it is
    /// correct, a hide is a miss, anything else is wrong.
    Update(&'static str),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
enum Verdict {
    Confirmed,
    Hidden,
    CorrectUpdate,
    FalseHide,
    MissedUpdate,
    FalseConfirm,
    WrongUpdate,
    SeedFailed,
}

fn judge(expect: Expect, r: &Resolution) -> Verdict {
    let proposed = match r {
        Resolution::Proposed(p) => Some(p.new_signature.as_str()),
        _ => None,
    };
    match expect {
        Expect::Same if r.confirms() => Verdict::Confirmed,
        Expect::Same if proposed.is_some() => Verdict::WrongUpdate,
        Expect::Same => Verdict::FalseHide,
        Expect::Gone if r.confirms() => Verdict::FalseConfirm,
        Expect::Gone if proposed.is_some() => Verdict::WrongUpdate,
        Expect::Gone => Verdict::Hidden,
        Expect::Update(_) if r.confirms() => Verdict::FalseConfirm,
        Expect::Update(want) => match proposed {
            Some(got) if got == want => Verdict::CorrectUpdate,
            Some(_) => Verdict::WrongUpdate,
            None => Verdict::MissedUpdate,
        },
    }
}

struct Case {
    name: &'static str,
    quote: &'static str,
    before: String,
    after: String,
    expect: Expect,
}

fn case(name: &'static str, quote: &'static str, before: &str, after: &str, expect: Expect) -> Case {
    Case {
        name,
        quote,
        before: page(before),
        after: page(after),
        expect,
    }
}

// ── page shapes ───────────────────────────────────────────────────────────

const STUDENT: &str = "학생 인증 시 10% 할인";

fn spa(inner: &str, depth: usize, banner: &str) -> String {
    let open: String = (0..depth)
        .map(|i| format!("<div class=\"css-{i}x r-{i}\" data-v-{i}>"))
        .collect();
    let close: String = (0..depth).map(|_| "</div>").collect();
    format!(
        "<div id=\"__next\"><!--$--><section class=\"sc-a\"><h2 class=\"sc-t\">학생 할인</h2>{banner}{open}{inner}{close}</section><!--/$--></div>"
    )
}

fn banner_body(banner: &str, body: &str) -> String {
    format!(
        "<div class=\"top-banner\"><a href=\"/e\"><span>{banner}</span></a></div>\
         <section><h2>학생 할인</h2><div class=\"benefit\"><p>{body}</p><p class=\"desc\">인증은 매 학기 갱신됩니다.</p></div></section>"
    )
}

fn colspan_table(second_header: &str) -> String {
    format!(
        "<section><h2>등급별 할인율</h2><table><thead>\
         <tr><th rowspan=\"2\">등급</th><th colspan=\"2\">할인율</th></tr>\
         <tr>{second_header}</tr></thead><tbody>\
         <tr><th>VIP</th><td>10% 할인</td><td>5% 할인</td></tr>\
         <tr><th>일반</th><td>3% 할인</td><td>1% 할인</td></tr></tbody></table></section>"
    )
}

fn rowspan_table(vip_store: &str, normal_store: &str) -> String {
    format!(
        "<section><h2>등급별 적립</h2><table><thead><tr><th>등급</th><th>구분</th><th>적립률</th></tr></thead><tbody>\
         <tr><th rowspan=\"2\">VIP</th><td>온라인</td><td>10% 적립</td></tr>\
         <tr><td>매장</td><td>{vip_store} 적립</td></tr>\
         <tr><th rowspan=\"2\">일반</th><td>온라인</td><td>5% 적립</td></tr>\
         <tr><td>매장</td><td>{normal_store} 적립</td></tr></tbody></table></section>"
    )
}

fn footnoted(statement: &str, footnote: &str) -> String {
    format!(
        "<section><h2>학생 할인</h2><p class=\"offer\">{statement}</p></section>\
         <div class=\"footnotes\"><p>{footnote}</p></div>"
    )
}

fn strike(price: &str) -> String {
    format!(
        "<section><h2>스탠다드 요금제</h2><div class=\"plan\"><p class=\"price\">{price}</p>\
         <p class=\"desc\">학생 인증 시 적용되는 요금입니다.</p></div></section>"
    )
}

/// Tier cards whose name sits in a head wrapper (or an icon), not beside
/// the statement.
fn wrapped_cards(cards: &[(&str, &str)]) -> String {
    let cards: String = cards
        .iter()
        .map(|(head, body)| {
            format!(
                "<div class=\"card\"><div class=\"card-head\">{head}</div><div class=\"card-body\"><p class=\"rate\">{body}</p></div></div>"
            )
        })
        .collect();
    format!("<section><h2>멤버십 등급 혜택</h2><div class=\"cards\">{cards}</div></section>")
}

fn labelled_cards(cards: &[(&str, &str, &str)]) -> String {
    let cards: String = cards
        .iter()
        .map(|(tier, body, note)| {
            format!(
                "<div class=\"card\"><strong class=\"tier\">{tier}</strong><p class=\"rate\">{body}</p>{note}</div>"
            )
        })
        .collect();
    format!("<section><h2>멤버십 등급 혜택</h2><div class=\"cards\">{cards}</div></section>")
}

const SCHOOLS: &[&str] = &[
    "서울대학교", "연세대학교", "고려대학교", "성균관대학교", "한양대학교", "중앙대학교", "경희대학교",
    "한국외국어대학교", "서강대학교", "이화여자대학교", "건국대학교", "동국대학교", "홍익대학교",
    "국민대학교", "숭실대학교", "세종대학교", "단국대학교", "인하대학교", "아주대학교", "가천대학교",
    "충남대학교", "충북대학교", "전남대학교", "전북대학교", "경북대학교", "강원대학교", "제주대학교",
    "울산대학교", "영남대학교", "계명대학교", "동아대학교", "부경대학교", "경성대학교", "동의대학교",
    "신라대학교", "인제대학교", "경남대학교", "창원대학교", "부산대학교",
];

fn schools(skip: Option<&str>) -> String {
    let items: String = SCHOOLS
        .iter()
        .filter(|s| Some(**s) != skip)
        .map(|s| format!("<li>{s}</li>"))
        .collect();
    format!(
        "<section><h2>제휴 대학 학생 할인</h2><p class=\"offer\">재학생 인증 시 10% 할인</p>\
         <ul class=\"schools\">{items}</ul></section>"
    )
}

fn campaign(period: &str) -> String {
    format!(
        "<section><h2>가을 신학기 이벤트</h2><p class=\"period\">이벤트 기간: {period}</p>\
         <p class=\"offer\">{STUDENT}</p></section>"
    )
}

fn far_note(note: &str) -> String {
    format!(
        "<section><h2>가을 신학기 이벤트</h2><p class=\"period\">{note}</p><p class=\"about\">{}</p>\
         <p class=\"offer\">{STUDENT}</p></section>",
        filler(4)
    )
}

fn with_note(note: &str) -> String {
    format!(
        "<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p><p class=\"target\">{note}</p></section>"
    )
}

fn terms(extra: &str) -> String {
    let items = [
        "할인은 결제 단계에서 자동으로 적용됩니다",
        "학생 인증은 매 학기 다시 받아야 합니다",
        "다른 쿠폰과 중복으로 사용할 수 있습니다",
        "환불 시 할인 금액을 뺀 금액이 환불됩니다",
        "인증 정보는 혜택 제공 목적으로만 사용됩니다",
        "자세한 내용은 고객센터로 문의해 주시기 바랍니다",
        "본 혜택은 제휴사 사정에 따라 변경될 수 있습니다",
        "앱과 웹에서 모두 같은 혜택을 받을 수 있습니다",
    ];
    let mut list: String = items.iter().map(|i| format!("<li>{i}</li>")).collect();
    list.push_str(extra);
    format!(
        "<section><h2>학생 할인</h2><p class=\"offer\">{STUDENT}</p>\
         <div class=\"terms\"><h3>유의사항</h3><ul>{list}</ul></div></section>"
    )
}

fn cases() -> Vec<Case> {
    let student = |p: &str| format!("<p class=\"offer\">{p}</p>");
    let vip5 = "결제 금액의 5% 적립";
    let normal1 = "결제 금액의 1% 적립";
    vec![
        // SPA-rendered remnants.
        case(
            "spa: hydration comments, unchanged",
            STUDENT,
            &spa("<p class=\"offer\">학생 인증 시 10<!-- -->% 할인</p>", 4, ""),
            &spa("<p class=\"offer\">학생 인증 시 10<!-- -->% 할인</p>", 4, ""),
            Expect::Same,
        ),
        case(
            "spa: comment moved, class hashes rotated",
            STUDENT,
            &spa("<p class=\"offer\">학생 인증 시 10<!-- -->% 할인</p>", 4, ""),
            &spa("<p class=\"o-9f\"><!-- -->학생 인증 시 <!-- -->10% 할인</p>", 4, "")
                .replace("css-", "jsx-"),
            Expect::Same,
        ),
        case(
            "spa: 9 wrapper levels, section banner '이벤트가 종료되었습니다.' (listed)",
            STUDENT,
            &spa(&student(STUDENT), 9, ""),
            &spa(&student(STUDENT), 9, "<div class=\"notice\">이벤트가 종료되었습니다.</div>"),
            Expect::Gone,
        ),
        case(
            "spa: 9 wrapper levels, section banner unlisted wording",
            STUDENT,
            &spa(&student(STUDENT), 9, ""),
            &spa(&student(STUDENT), 9, "<div class=\"notice\">이 행사는 막을 내렸습니다.</div>"),
            Expect::Gone,
        ),
        case(
            "spa: content streamed into hidden S:0, fallback visible",
            STUDENT,
            &spa(&student(STUDENT), 3, ""),
            &format!(
                "<div id=\"__next\"><p>로딩 중</p></div><div hidden id=\"S:0\">{}</div>",
                spa(&student(STUDENT), 3, "")
            ),
            Expect::Gone,
        ),
        case(
            "spa: new value rendered, old copy in noscript",
            STUDENT,
            &spa(&student(STUDENT), 3, ""),
            &format!(
                "{}<noscript><p>{STUDENT}</p></noscript>",
                spa(&student("학생 인증 시 15% 할인"), 3, "")
            ),
            Expect::Update("15%"),
        ),
        // Offer twice: banner + body.
        case(
            "twice: banner + body unchanged",
            STUDENT,
            &banner_body(STUDENT, STUDENT),
            &banner_body(STUDENT, STUDENT),
            Expect::Same,
        ),
        case(
            "twice: body 15%, banner still 10%",
            STUDENT,
            &banner_body(STUDENT, STUDENT),
            &banner_body(STUDENT, "학생 인증 시 15% 할인"),
            Expect::Gone,
        ),
        case(
            "twice: banner 15%, body still 10%",
            STUDENT,
            &banner_body(STUDENT, STUDENT),
            &banner_body("학생 인증 시 15% 할인", STUDENT),
            Expect::Gone,
        ),
        case(
            "twice: banner removed",
            STUDENT,
            &banner_body(STUDENT, STUDENT),
            &format!(
                "<section><h2>학생 할인</h2><div class=\"benefit\"><p>{STUDENT}</p><p class=\"desc\">인증은 매 학기 갱신됩니다.</p></div></section>"
            ),
            Expect::Same,
        ),
        // Comparison tables with merged cells.
        case(
            "table colspan: unchanged",
            "10% 할인",
            &colspan_table("<th>온라인</th><th>매장</th>"),
            &colspan_table("<th>온라인</th><th>매장</th>"),
            Expect::Same,
        ),
        case(
            "table colspan: sub-header 온라인/매장 swapped (VIP online now 5%)",
            "10% 할인",
            &colspan_table("<th>온라인</th><th>매장</th>"),
            &colspan_table("<th>매장</th><th>온라인</th>"),
            Expect::Update("5%"),
        ),
        case(
            "table rowspan: unchanged (VIP store 7%)",
            "7% 적립",
            &rowspan_table("7%", "3%"),
            &rowspan_table("7%", "3%"),
            Expect::Same,
        ),
        case(
            "table rowspan: VIP store 7% <-> 일반 store 3% swapped",
            "7% 적립",
            &rowspan_table("7%", "3%"),
            &rowspan_table("3%", "7%"),
            Expect::Update("3%"),
        ),
        // Footnote markers.
        case(
            "footnote: unchanged",
            "학생 인증 시 10% 할인※",
            &footnoted("학생 인증 시 10% 할인※", "※ 온라인 몰 결제 시 적용"),
            &footnoted("학생 인증 시 10% 할인※", "※ 온라인 몰 결제 시 적용"),
            Expect::Same,
        ),
        case(
            "footnote: ※ text now '신규 회원에 한함' (listed, below the section)",
            "학생 인증 시 10% 할인※",
            &footnoted("학생 인증 시 10% 할인※", "※ 온라인 몰 결제 시 적용"),
            &footnoted("학생 인증 시 10% 할인※", "※ 신규 회원에 한함"),
            Expect::Gone,
        ),
        case(
            "footnote: *1 added to the statement",
            STUDENT,
            &footnoted(STUDENT, "고객센터에서 인증을 도와드립니다"),
            &footnoted("학생 인증 시 10% 할인*1", "*1 일부 매장은 대상에서 빠집니다"),
            Expect::Gone,
        ),
        // Strikethrough + new price.
        case(
            "strike: unchanged",
            "월 10,900원",
            &strike("<del>정가 13,500원</del> <strong>월 10,900원</strong>"),
            &strike("<del>정가 13,500원</del> <strong>월 10,900원</strong>"),
            Expect::Same,
        ),
        case(
            "strike: new sale price 9,900",
            "월 10,900원",
            &strike("<del>정가 13,500원</del> <strong>월 10,900원</strong>"),
            &strike("<del>정가 13,500원</del> <strong>월 9,900원</strong>"),
            Expect::Update("9900원"),
        ),
        case(
            "strike: promotion over, regular price only",
            "월 10,900원",
            &strike("<del>정가 13,500원</del> <strong>월 10,900원</strong>"),
            &strike("<strong>월 13,500원</strong>"),
            Expect::Update("13500원"),
        ),
        case(
            "strike: fact price now struck (line-through span), new price beside",
            "월 10,900원",
            &strike("<span class=\"now\">월 10,900원</span>"),
            &strike("<span class=\"line-through\">월 10,900원</span> <span class=\"now\">월 8,900원</span>"),
            Expect::Update("8900원"),
        ),
        // Member tiers.
        case(
            "tiers wrapped: unchanged",
            vip5,
            &wrapped_cards(&[("<span>VIP</span>", vip5), ("<span>일반</span>", normal1)]),
            &wrapped_cards(&[("<span>VIP</span>", vip5), ("<span>일반</span>", normal1)]),
            Expect::Same,
        ),
        case(
            "tiers wrapped: VIP 5% <-> 일반 1% swapped",
            vip5,
            &wrapped_cards(&[("<span>VIP</span>", vip5), ("<span>일반</span>", normal1)]),
            &wrapped_cards(&[("<span>VIP</span>", normal1), ("<span>일반</span>", vip5)]),
            Expect::Update("1%"),
        ),
        case(
            "tiers icon badges (no text): VIP 5% <-> 일반 1% swapped",
            vip5,
            &wrapped_cards(&[("<i class=\"badge-vip\"></i>", vip5), ("<i class=\"badge-normal\"></i>", normal1)]),
            &wrapped_cards(&[("<i class=\"badge-vip\"></i>", normal1), ("<i class=\"badge-normal\"></i>", vip5)]),
            Expect::Update("1%"),
        ),
        case(
            "tiers labelled: VIP 5% <-> 일반 1% swapped (control)",
            vip5,
            &labelled_cards(&[("VIP", vip5, ""), ("일반", normal1, "")]),
            &labelled_cards(&[("VIP", normal1, ""), ("일반", vip5, "")]),
            Expect::Update("1%"),
        ),
        case(
            "tiers labelled: note '※ 첫 결제 고객 전용' moves 일반 -> VIP",
            vip5,
            &labelled_cards(&[("VIP", vip5, ""), ("일반", normal1, "<p class=\"note\">※ 첫 결제 고객 전용</p>")]),
            &labelled_cards(&[("VIP", vip5, "<p class=\"note\">※ 첫 결제 고객 전용</p>"), ("일반", normal1, "")]),
            Expect::Gone,
        ),
        case(
            "tiers wrapped: tier names swap slots, 일반 slot now 3% (wrong-update bait)",
            vip5,
            &wrapped_cards(&[("<span>VIP</span>", vip5), ("<span>일반</span>", "구매 금액의 1% 적립")]),
            &wrapped_cards(&[("<span>일반</span>", "결제 금액의 3% 적립"), ("<span>VIP</span>", "구매 금액의 1% 적립")]),
            Expect::Gone,
        ),
        // Per-school pages.
        case(
            "schools: unchanged long list",
            "재학생 인증 시 10% 할인",
            &schools(None),
            &schools(None),
            Expect::Same,
        ),
        case(
            "schools: 서울대학교 (start of list) removed",
            "재학생 인증 시 10% 할인",
            &schools(None),
            &schools(Some("서울대학교")),
            Expect::Gone,
        ),
        case(
            "schools: 부산대학교 (end of a >200-char list) removed",
            "재학생 인증 시 10% 할인",
            &schools(None),
            &schools(Some("부산대학교")),
            Expect::Gone,
        ),
        case(
            "schools: per-school headings, unchanged",
            "재학생 인증 시 10% 할인",
            "<section><h2>부산대학교</h2><p>재학생 인증 시 10% 할인</p></section><section><h2>부경대학교</h2><p>재학생 인증 시 10% 할인</p></section>",
            "<section><h2>부경대학교</h2><p>재학생 인증 시 10% 할인</p></section><section><h2>부산대학교</h2><p>재학생 인증 시 10% 할인</p></section>",
            Expect::Same,
        ),
        // Seasonal campaigns and date-only changes.
        case(
            "season: '2026.09.01 ~ 10.31' -> '~ 10.13' (ended; anagram digits)",
            STUDENT,
            &campaign("2026.09.01 ~ 10.31"),
            &campaign("2026.09.01 ~ 10.13"),
            Expect::Gone,
        ),
        case(
            "season: '~ 2026.10.31' -> '~ 2026.10.13' (full date, control)",
            STUDENT,
            &campaign("2026.09.01 ~ 2026.10.31"),
            &campaign("2026.09.01 ~ 2026.10.13"),
            Expect::Gone,
        ),
        case(
            "season: '9월 1일 ~ 10월 31일' -> '~ 10월 15일' far from the quote (ended)",
            STUDENT,
            &far_note("이벤트 기간: 2026년 9월 1일 ~ 10월 31일"),
            &far_note("이벤트 기간: 2026년 9월 1일 ~ 10월 15일"),
            Expect::Gone,
        ),
        case(
            "date only: extended '~ 2026.10.31' -> '~ 2026.12.31'",
            STUDENT,
            &campaign("2026.09.01 ~ 2026.10.31"),
            &campaign("2026.09.01 ~ 2026.12.31"),
            Expect::Same,
        ),
        case(
            "date only: '~ 2026.12.31' -> '~ 2026.12.13' (future, anagram)",
            STUDENT,
            &campaign("2026.09.01 ~ 2026.12.31"),
            &campaign("2026.09.01 ~ 2026.12.13"),
            Expect::Same,
        ),
        case(
            "date only: season word 가을 -> 겨울 in heading",
            STUDENT,
            &campaign("2026.09.01 ~ 2026.12.31"),
            &campaign("2026.09.01 ~ 2026.12.31").replace("가을", "겨울"),
            Expect::Same,
        ),
        // Another offer adopts the old wording.
        case(
            "adopt: student section removed, 직장인 section now reads the old sentence",
            STUDENT,
            &format!("<section><h2>학생 할인</h2>{}</section><section><h2>직장인 할인</h2><p>재직 인증 시 5% 할인</p></section>", student(STUDENT)),
            &format!("<section><h2>직장인 할인</h2>{}</section>", student(STUDENT)),
            Expect::Gone,
        ),
        case(
            "adopt: same heading, sentence now under a partner-card block title",
            STUDENT,
            &format!("<section><h2>학생 할인</h2>{}</section>", student(STUDENT)),
            &format!("<section><h2>학생 할인</h2><div class=\"partner\"><div class=\"title\">OO카드 결제 고객</div>{}</div></section>", student(STUDENT)),
            Expect::Gone,
        ),
        // Value moved into an image / a tooltip.
        case(
            "image: value moved into <img alt>",
            STUDENT,
            &with_note("인증은 매 학기 갱신됩니다."),
            &format!("<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 <img src=\"/r.png\" alt=\"10%\"> 할인</p><p class=\"target\">인증은 매 학기 갱신됩니다.</p></section>"),
            Expect::Gone,
        ),
        case(
            "tooltip: value moved into title attribute",
            STUDENT,
            &with_note("인증은 매 학기 갱신됩니다."),
            &format!("<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 <span title=\"10% 할인\">할인</span></p><p class=\"target\">인증은 매 학기 갱신됩니다.</p></section>"),
            Expect::Gone,
        ),
        case(
            "tooltip: condition added only in a title attribute",
            STUDENT,
            &with_note("인증은 매 학기 갱신됩니다."),
            &format!("<section><h2>학생 할인</h2><p class=\"offer\">학생 인증 시 10% <abbr title=\"매장 결제는 대상에서 빠집니다\">할인</abbr></p><p class=\"target\">인증은 매 학기 갱신됩니다.</p></section>"),
            Expect::Gone,
        ),
        // Surrounding-text digest attacks (sorted characters stay equal).
        case(
            "digest: audience swap '적용: 대학생 / 미적용: 대학원생'",
            STUDENT,
            &with_note("적용 대상: 대학생 / 미적용: 대학원생"),
            &with_note("적용 대상: 대학원생 / 미적용: 대학생"),
            Expect::Gone,
        ),
        case(
            "digest: channel swap '온라인 적용, 매장 미적용'",
            STUDENT,
            &with_note("온라인 결제 시 적용, 매장 결제 시 미적용"),
            &with_note("매장 결제 시 적용, 온라인 결제 시 미적용"),
            Expect::Gone,
        ),
        case(
            "digest: limit digits '1인당 3매까지' -> '3인당 1매까지'",
            STUDENT,
            &with_note("1인당 3매까지 구매 가능"),
            &with_note("3인당 1매까지 구매 가능"),
            Expect::Gone,
        ),
        case(
            "digest: cap '최대 5,000원' -> '최대 500원' beyond the 150-char window",
            STUDENT,
            &far_note("할인 한도: 최대 5,000원"),
            &far_note("할인 한도: 최대 500원"),
            Expect::Gone,
        ),
        case(
            "digest: restriction appended after char 200 of a long 유의사항 list",
            STUDENT,
            &terms(""),
            &terms("<li>학생 인증은 국내 대학 재학생만 가능합니다</li>"),
            Expect::Gone,
        ),
        case(
            "scope: card repeats heading, section banner '이벤트가 종료되었습니다.' (listed)",
            STUDENT,
            &format!("<section><h2>학생 할인</h2><div class=\"card\"><p class=\"t\">학생 할인</p>{}</div></section>", student(STUDENT)),
            &format!("<section><h2>학생 할인</h2><div class=\"notice\">이벤트가 종료되었습니다.</div><div class=\"card\"><p class=\"t\">학생 할인</p>{}</div></section>", student(STUDENT)),
            Expect::Gone,
        ),
        case(
            "scope: statement contains heading text, section banner '이벤트가 종료되었습니다.'",
            "학생 인증 시 10% 할인 혜택",
            "<section><h2>할인 혜택</h2><div class=\"box\"><p>학생 인증 시 10% 할인 혜택</p></div></section>",
            "<section><h2>할인 혜택</h2><div class=\"notice\">이벤트가 종료되었습니다.</div><div class=\"box\"><p>학생 인증 시 10% 할인 혜택</p></div></section>",
            Expect::Gone,
        ),
        case(
            "season: period after the offer, '2026.09.01 ~ 10.31' -> '~ 10.13' (ended; anagram digits)",
            STUDENT,
            &campaign_after("2026.09.01 ~ 10.31"),
            &campaign_after("2026.09.01 ~ 10.13"),
            Expect::Gone,
        ),
        case(
            "season: period after the offer, '9월 1일 ~ 10월 31일' -> '~ 10월 15일' (ended)",
            STUDENT,
            &campaign_after("2026년 9월 1일 ~ 10월 31일"),
            &campaign_after("2026년 9월 1일 ~ 10월 15일"),
            Expect::Gone,
        ),
        case(
            "date only: period after the offer, '~ 2026.12.31' -> '~ 2026.12.13' (future, anagram)",
            STUDENT,
            &campaign_after("2026.09.01 ~ 2026.12.31"),
            &campaign_after("2026.09.01 ~ 2026.12.13"),
            Expect::Same,
        ),
        case(
            "table colspan: sub-header swapped AND VIP col-1 10% -> 12% (col 1 is now 매장)",
            "10% 할인",
            &colspan_table("<th>온라인</th><th>매장</th>"),
            &colspan_table("<th>매장</th><th>온라인</th>").replace("<td>10% 할인</td>", "<td>12% 할인</td>"),
            Expect::Update("5%"),
        ),
    ]
}

fn campaign_after(period: &str) -> String {
    format!(
        "<section><h2>가을 신학기 이벤트</h2><p class=\"offer\">{STUDENT}</p>\
         <p class=\"period\">이벤트 기간: {period}</p></section>"
    )
}

/// Pages already ended at first sight: the seed itself must not confirm.
fn seed_cases() -> Vec<(&'static str, String)> {
    vec![
        ("seed: '2026.09.01 ~ 10.13'", page(&campaign("2026.09.01 ~ 10.13"))),
        ("seed: '2026년 9월 1일 ~ 10월 13일'", page(&campaign("2026년 9월 1일 ~ 10월 13일"))),
        ("seed: '9.1 ~ 10.13'", page(&campaign("9.1 ~ 10.13"))),
        ("seed: '2026.09.01 - 2026.10.13' (control)", page(&campaign("2026.09.01 - 2026.10.13"))),
        ("seed: '10월 13일까지' (control)", page(&campaign("10월 13일까지"))),
    ]
}

#[test]
fn r5_adversarial_replay() {
    let mut rows = Vec::new();
    for c in cases() {
        let verdict = match seed(&c.before, c.quote) {
            Err(why) => {
                println!("SEED_FAILED {}: {why}", c.name);
                Verdict::SeedFailed
            }
            Ok(stored) => {
                let r = after(&stored, &c.after);
                let v = judge(c.expect, &r);
                println!("{v:?} | {} | expect {:?} | got {}: {r:?}", c.name, c.expect, r.label());
                v
            }
        };
        rows.push((c.name, verdict));
    }
    for (name, html) in seed_cases() {
        let v = view(&html);
        let empty = Anchors::default();
        let r = resolve(&v, &fact(STUDENT, None, &empty, true));
        let verdict = if r.confirms() {
            Verdict::FalseConfirm
        } else {
            Verdict::Hidden
        };
        println!("{verdict:?} | {name} | expect not confirmed | got {}", r.label());
        rows.push((name, verdict));
    }
    let mut counts = std::collections::BTreeMap::new();
    for (_, v) in &rows {
        *counts.entry(*v).or_insert(0usize) += 1;
    }
    println!("R5 TOTAL {} verdicts: {counts:?}", rows.len());
    let bad: Vec<&str> = rows
        .iter()
        .filter(|(_, v)| matches!(v, Verdict::FalseConfirm | Verdict::WrongUpdate))
        .map(|(n, _)| *n)
        .collect();
    assert!(bad.is_empty(), "false confirms / wrong updates / seed failures: {bad:#?}");
}

/// The surrounding digest alone (white-box through the public capture):
/// meaningful edits whose sorted characters are equal keep the digest.
#[test]
fn r5_digest_collisions() {
    let pairs = [
        ("적용 대상: 대학생 / 미적용: 대학원생", "적용 대상: 대학원생 / 미적용: 대학생"),
        ("온라인 결제 시 적용, 매장 결제 시 미적용", "매장 결제 시 적용, 온라인 결제 시 미적용"),
        ("1인당 3매까지 구매 가능", "3인당 1매까지 구매 가능"),
        ("평일 10시부터 18시까지", "평일 18시부터 10시까지"),
        ("최대 5,000원", "최대 50,000원"),
        ("신규 회원 대상", "회원 신규 대상"),
    ];
    let mut collisions = Vec::new();
    for (before, after) in pairs {
        let notes = |note: &str| {
            let html = page(&with_note(note));
            let v = view(&html);
            capture(&v, STUDENT)
                .expect("holder")
                .identities
                .iter()
                .map(|i| i.notes.clone())
                .collect::<Vec<_>>()
        };
        let same = notes(before) == notes(after);
        println!("digest {} | {before} -> {after}", if same { "COLLIDES" } else { "differs" });
        if same {
            collisions.push((before, after));
        }
    }
    println!("R5 DIGEST collisions: {} of {}", collisions.len(), pairs.len());
}

