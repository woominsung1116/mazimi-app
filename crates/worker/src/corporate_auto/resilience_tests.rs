use super::*;
use crate::corporate_auto::evidence::visible_text;

const QUOTE: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
const AGE_QUOTE: &str = "만 34세 이하 고객이 가입할 수 있습니다.";

fn shell(body: &str) -> String {
    format!(
        "<html><head><title>t</title></head><body><nav class=\"gnb\"><a href=\"/\">홈</a><a href=\"/student\">학생 혜택</a></nav>\
         <div class=\"container\">{body}</div><footer>고객센터 1588-0000</footer></body></html>"
    )
}

/// The page as it was when the facts were first confirmed.
fn original() -> String {
    shell(
        "<section class=\"benefit-section\" id=\"student\"><h2 class=\"title\">학생 할인</h2>\
         <div class=\"benefit-body\"><p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>\
         <p class=\"note\">30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p></div></section>\
         <section class=\"age\"><h2>가입 대상</h2><div><p class=\"cond\">만 34세 이하 고객이 가입할 수 있습니다.</p></div></section>\
         <section class=\"other\"><h2>멤버십</h2><p>멤버십 회원은 매월 2,000원 적립</p></section>",
    )
}

pub(super) fn view(html: &str) -> PageView {
    PageView::new(html, &visible_text(html))
}

/// A fact as stored after its first confirmation: the adopted window
/// baseline and the captured fingerprint with identities.
pub(super) struct Confirmed {
    pub(super) quote: String,
    pub(super) baseline: String,
    pub(super) anchors: Anchors,
    pub(super) ages: Ages,
}

pub(super) fn confirm_on(html: &str, quote: &str, ages: Ages) -> Option<Confirmed> {
    confirm_in(&view(html), quote, ages)
}

pub(super) fn confirm_in(page: &PageView, quote: &str, ages: Ages) -> Option<Confirmed> {
    let FactOutcome::Confirmed {
        window_signature, ..
    } = evaluate_fact(&page.evidence, quote, None, ages, true)
    else {
        return None;
    };
    Some(Confirmed {
        quote: quote.to_string(),
        baseline: window_signature,
        anchors: Anchors {
            fingerprint: capture(page, quote),
            ..Anchors::default()
        },
        ages,
    })
}

fn confirmed(quote: &str, ages: Ages) -> Confirmed {
    confirm_on(&original(), quote, ages).expect("the original page confirms the quote")
}

fn no_ages() -> Ages {
    Ages {
        min: None,
        max: None,
    }
}

fn kt_ages() -> Ages {
    Ages {
        min: None,
        max: Some(34),
    }
}

pub(super) fn resolve_on(fact: &Confirmed, html: &str) -> Resolution {
    resolve_in(fact, &view(html))
}

/// What the shadow value matcher reads for `fact` on `html`.
fn shadow_on(fact: &Confirmed, html: &str) -> Option<Proposal> {
    shadow_update(
        &view(html),
        &StoredFact {
            quote: &fact.quote,
            baseline: Some(&fact.baseline),
            ages: fact.ages,
            adoption_open: false,
            anchors: &fact.anchors,
        },
    )
}

/// Hidden on its own evidence: the quote is gone, or its value window or
/// a field reads otherwise (never because the page changed elsewhere).
fn assert_hidden_on_evidence(resolution: &Resolution) {
    assert!(
        matches!(
            resolution,
            Resolution::Exact(
                FactOutcome::QuoteMissing
                    | FactOutcome::WindowChanged
                    | FactOutcome::FieldMismatch(_)
            )
        ),
        "{resolution:?}"
    );
}

pub(super) fn resolve_in(fact: &Confirmed, page: &PageView) -> Resolution {
    resolve(
        page,
        &StoredFact {
            quote: &fact.quote,
            baseline: Some(&fact.baseline),
            ages: fact.ages,
            adoption_open: false,
            anchors: &fact.anchors,
        },
    )
}

// ── normalization, values, second extractor ───────────────────────────────

#[test]
fn folding_tolerates_width_spacing_and_punctuation_but_keeps_numbers() {
    assert_eq!(
        fold_str("학생 인증 후 １０％ 할인!\n혜택을 제공해 드립니다"),
        fold_str("학생 인증 후 10% 할인, 혜택을 제공해 드립니다.")
    );
    assert_eq!(fold_str("월 6,600원"), "월6600원");
    assert_eq!(fold_str("19～34세"), "19~34세");
    assert_eq!(fold_str("1.5 GB"), "1.5gb");
    assert_ne!(fold_str("1.5%"), fold_str("15%"));
}

#[test]
fn identity_sentence_masks_only_the_one_value() {
    assert_eq!(
        identity_sentence("학생 10% 할인").as_deref(),
        Some("학생#%할인")
    );
    assert_eq!(
        identity_sentence("1인 요금제: 월 3,000원 할인").as_deref(),
        Some("1인요금제월#원할인")
    );
    assert_eq!(
        identity_sentence("만 34세 이하 고객").as_deref(),
        Some("만#세이하고객")
    );
    assert_eq!(
        identity_sentence("만 19세~34세 고객"),
        None,
        "a range states two values"
    );
    assert_ne!(
        identity_sentence("1인 요금제 3,000원"),
        identity_sentence("2인 요금제 3,000원")
    );
    assert_eq!(
        identity_sentence("12개월 약정 시 월 3,000원 할인"),
        None,
        "two values: no proposal"
    );
    assert_eq!(identity_sentence("학생 할인"), None);
}

#[test]
fn the_second_extractor_reads_the_same_values_independently() {
    for text in [
        "학생 10% 할인, 월 6,600원, 최대 48개월",
        "만 19세~34세 고객",
        "1.5GB 데이터와 300P 적립, 3 months free",
        "2026년 6월 23일까지",
        "포인트 없음",
        "월 1만 원 할인",
    ] {
        assert_eq!(values_b(text), value_signature(text), "{text}");
    }
    assert_eq!(
        value_signature("월 1만 원 할인"),
        value_signature("월 1만원 할인")
    );
    assert!(extractors_agree("만 34세 이하 고객이 가입할 수 있습니다."));
    assert!(extractors_agree(
        "학생 인증 후 15% 할인 혜택을 제공해 드립니다."
    ));
}

// ── redesigns that keep the offer: confirmed ──────────────────────────────

#[test]
fn class_and_id_renames_still_confirm() {
    let fact = confirmed(QUOTE, no_ages());
    let renamed = original()
        .replace("benefit-section", "sc-1x9f2")
        .replace("id=\"student\"", "id=\"promo-a\"")
        .replace("class=\"desc\"", "class=\"Text_body__3kd\"");
    let resolution = resolve_on(&fact, &renamed);
    assert!(resolution.confirms(), "{resolution:?}");
}

#[test]
fn wrapped_markup_confirms_and_a_reordered_offer_section_is_held() {
    let fact = confirmed(QUOTE, no_ages());
    let reordered = shell(
        "<section class=\"other\"><h2>멤버십</h2><p>멤버십 회원은 매월 2,000원 적립</p></section>\
         <section class=\"age\"><h2>가입 대상</h2><div><p class=\"cond\">만 34세 이하 고객이 가입할 수 있습니다.</p></div></section>\
         <section class=\"benefit-section\"><h2 class=\"title\">학생 할인</h2><div class=\"benefit-body\">\
         <p class=\"note\">30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p>\
         <p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p></div></section>",
    );
    // The offer's own section reads in another order: held for fresh
    // extraction, which re-reads it.
    assert_eq!(
        resolve_on(&fact, &reordered),
        Resolution::Ambiguous(SECTION_CHANGED.into())
    );
    let wrapped = original().replace(
        "<p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>",
        "<div class=\"card__inner\"><p><span>학생 인증 후 <strong>10%</strong> 할인</span>\n<em>혜택을</em> 제공해 드립니다.</p></div>",
    );
    assert!(resolve_on(&fact, &wrapped).confirms());
}

#[test]
fn width_and_line_break_changes_confirm_by_the_exact_anchor() {
    let fact = confirmed(QUOTE, no_ages());
    let retyped = original().replace(QUOTE, "학생 인증 후 １０％ 할인 혜택을<br>제공해 드립니다.");
    let resolution = resolve_on(&fact, &retyped);
    // Width and line breaks are the same canonical text.
    assert_eq!(resolution.label(), "confirmed", "{resolution:?}");
    // Punctuation is page text: a changed mark is a changed page.
    let exclaimed = original().replace(QUOTE, "학생 인증 후 10% 할인 혜택을 제공해 드립니다!");
    assert_hidden_on_evidence(&resolve_on(&fact, &exclaimed));
}

/// A structural redesign (new wrappers, classes, heading level) with the
/// same copy and the same headings is the same offer: relocated.
#[test]
fn a_structural_redesign_with_the_same_copy_is_relocated() {
    let fact = confirmed(QUOTE, no_ages());
    let redesign = shell(
        "<main class=\"layout\"><section class=\"promo\"><h3 class=\"promo__title\">학생 할인</h3>\
         <div class=\"promo__text\"><div><span>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</span></div>\
         <div><span>30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</span></div></div></section>\
         <section class=\"promo\"><h3>가입 대상</h3><div><span>만 34세 이하 고객이 가입할 수 있습니다.</span></div></section>\
         <section class=\"promo\"><h3>멤버십</h3><p>멤버십 회원은 매월 2,000원 적립</p></section></main>",
    );
    let resolution = resolve_on(&fact, &redesign);
    assert_eq!(resolution.label(), "relocated", "{resolution:?}");
}

// ── changes: proposed only when provably this offer's new value ───────────

#[test]
fn a_changed_number_hides_and_is_only_a_shadow_update() {
    let fact = confirmed(QUOTE, no_ages());
    let changed = original().replace("10% 할인", "15% 할인");
    assert_hidden_on_evidence(&resolve_on(&fact, &changed));
    let proposal = shadow_on(&fact, &changed).expect("the matcher reads the change");
    assert_eq!(
        (
            proposal.old_signature.as_str(),
            proposal.new_signature.as_str()
        ),
        ("10%", "15%")
    );
    assert!(proposal.extractors_agree);
    assert_eq!(
        proposal.new_text,
        "학생 인증 후 15% 할인 혜택을 제공해 드립니다."
    );

    let age = confirmed(AGE_QUOTE, kt_ages());
    let younger = original().replace("만 34세 이하", "만 29세 이하");
    assert_hidden_on_evidence(&resolve_on(&age, &younger));
    let proposal = shadow_on(&age, &younger).expect("an age change is read");
    assert_eq!(proposal.new_ages, Some((None, Some(29))));
}

#[test]
fn rewritten_copy_is_hidden_for_fresh_extraction() {
    let fact = confirmed(QUOTE, no_ages());
    for rewrite in [
        "학생 인증 후 10% 할인 혜택을 드립니다.",
        "학생 인증 후 15% 할인 혜택을 드립니다.",
        "학생이라면 인증만 하면 10% 할인을 받을 수 있어요.",
        "대학원생 인증 후 10% 할인 혜택을 제공해 드립니다.",
        "학생 인증 후 10% 적립 혜택을 제공해 드립니다.",
        "학생 인증 후 10%p 할인 혜택을 제공해 드립니다.",
    ] {
        let resolution = resolve_on(&fact, &original().replace(QUOTE, rewrite));
        assert!(!resolution.confirms(), "{rewrite}: {resolution:?}");
        assert!(
            !matches!(resolution, Resolution::Proposed(_)),
            "{rewrite}: {resolution:?}"
        );
    }
    let age = confirmed(AGE_QUOTE, kt_ages());
    for rewrite in [
        "만 34세 이상 고객이 가입할 수 있습니다.",
        "만 34세 이하 고객이 가입할 수 없습니다.",
    ] {
        assert!(!resolve_on(&age, &original().replace(AGE_QUOTE, rewrite)).confirms());
    }
}

fn tiers(heading: &str, body: &str) -> String {
    shell(&format!(
        "<section class=\"plans\"><h2>{heading}</h2>{body}</section>"
    ))
}

fn assert_no_swap(quote: &str, before: &str, after: &str) {
    let fact = confirm_on(before, quote, no_ages()).expect("confirms before");
    let resolution = resolve_on(&fact, after);
    assert!(!resolution.confirms(), "{quote}: {resolution:?}");
    assert!(
        !matches!(resolution, Resolution::Proposed(_)),
        "{quote}: {resolution:?}"
    );
}

/// Round 3 CRITICAL: a tier label in a heading, th, dt or card title.
#[test]
fn a_tier_label_outside_the_sentence_never_lends_its_value() {
    let quote = "월 3,000원 할인";
    let rows = |tiers_html: &str| tiers("학생 요금제", tiers_html);
    assert_no_swap(
        quote,
        &rows("<h3>1인 요금제</h3><p>월 3,000원 할인</p><h3>2인 요금제</h3><p>월 5,000원 할인</p>"),
        &rows("<h3>2인 요금제</h3><p>월 5,000원 할인</p>"),
    );
    assert_no_swap(
        quote,
        &rows("<table><tr><th>1인</th><td>월 3,000원 할인</td></tr><tr><th>2인</th><td>월 5,000원 할인</td></tr></table>"),
        &rows("<table><tr><th>2인</th><td>월 5,000원 할인</td></tr></table>"),
    );
    assert_no_swap(
        "구매 금액의 5% 적립",
        &rows("<dl><dt>Lv.1</dt><dd>구매 금액의 5% 적립</dd><dt>Lv.2</dt><dd>구매 금액의 7% 적립</dd></dl>"),
        &rows("<dl><dt>Lv.2</dt><dd>구매 금액의 7% 적립</dd></dl>"),
    );
    assert_no_swap(
        quote,
        &rows("<h4>1개월 이용권</h4><p>월 3,000원 할인</p><h4>3개월 이용권</h4><p>월 5,000원 할인</p>"),
        &rows("<h4>3개월 이용권</h4><p>월 5,000원 할인</p>"),
    );
    assert_no_swap(
        quote,
        &rows("<div class=\"card\"><div class=\"card__name\">Basic</div><p>월 3,000원 할인</p></div><div class=\"card\"><div class=\"card__name\">Pro</div><p>월 5,000원 할인</p></div>"),
        &rows("<div class=\"card\"><div class=\"card__name\">Pro</div><p>월 5,000원 할인</p></div>"),
    );
    // Column headers count too.
    assert_no_swap(
        quote,
        &rows("<table><tr><th>구분</th><th>1인</th><th>2인</th></tr><tr><th>학생</th><td>월 3,000원 할인</td><td>월 5,000원 할인</td></tr></table>"),
        &rows("<table><tr><th>구분</th><th>2인</th></tr><tr><th>학생</th><td>월 5,000원 할인</td></tr></table>"),
    );
}

/// Round 3 HIGH: the same value under another tier is not this offer.
#[test]
fn the_same_value_under_another_tier_never_confirms() {
    let rows = |tiers_html: &str| tiers("학생 요금제", tiers_html);
    assert_no_swap(
        "월 3,000원 할인",
        &rows("<h3>1인 요금제</h3><p>월 3,000원 할인</p><h3>2인 요금제</h3><p>월 3,000원 할인</p>"),
        &rows("<h3>2인 요금제</h3><p>월 3,000원 할인</p>"),
    );
    for (one, two) in [("1인", "2인"), ("1단계", "2단계"), ("코드 01", "코드 02")] {
        let quote = format!("{one} 요금제: 월 3,000원 할인");
        assert_no_swap(
            &quote,
            &rows(&format!(
                "<ul><li>{quote}</li><li>{two} 요금제: 월 3,000원 할인</li></ul>"
            )),
            &rows(&format!("<ul><li>{two} 요금제: 월 3,000원 할인</li></ul>")),
        );
    }
    // The stored tier still there with the same sibling price: confirmed.
    let quote = "월 3,000원 할인";
    let before =
        rows("<h3>1인 요금제</h3><p>월 3,000원 할인</p><h3>2인 요금제</h3><p>월 3,000원 할인</p>");
    let fact = confirm_on(&before, quote, no_ages()).unwrap();
    assert!(resolve_on(&fact, &before).confirms());
}

#[test]
fn numbered_tier_price_change_of_the_same_tier_is_read_in_shadow() {
    let rows = |tiers_html: &str| tiers("학생 요금제", tiers_html);
    let quote = "월 3,000원 할인";
    let before =
        rows("<h3>1인 요금제</h3><p>월 3,000원 할인</p><h3>2인 요금제</h3><p>월 5,000원 할인</p>");
    let after =
        rows("<h3>1인 요금제</h3><p>월 3,500원 할인</p><h3>2인 요금제</h3><p>월 5,000원 할인</p>");
    let fact = confirm_on(&before, quote, no_ages()).unwrap();
    assert_hidden_on_evidence(&resolve_on(&fact, &after));
    let proposal = shadow_on(&fact, &after).expect("a price change of the same tier is read");
    assert_eq!(proposal.new_signature, "3500원");
}

#[test]
fn a_hidden_duplicate_in_an_inactive_tab_never_confirms_the_old_value() {
    let fact = confirmed(QUOTE, no_ages());
    for hidden_tab in [
        "<div class=\"tab-pane\" id=\"old\"><p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p></div>",
        "<div role=\"tabpanel\" hidden><p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p></div>",
        "<div class=\"panel\" aria-hidden=\"true\"><p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p></div>",
    ] {
        let tabs = original().replace(
            "<p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>",
            &format!(
                "<div class=\"tabs\"><div class=\"tab-pane active\" id=\"now\"><p class=\"desc\">학생 인증 후 15% 할인 혜택을 제공해 드립니다.</p></div>{hidden_tab}</div>"
            ),
        );
        let resolution = resolve_on(&fact, &tabs);
        assert!(!resolution.confirms(), "{hidden_tab}: {resolution:?}");
        assert_hidden_on_evidence(&resolution);
        let read = shadow_on(&fact, &tabs).map(|p| p.new_signature);
        assert_eq!(read.as_deref(), Some("15%"), "{hidden_tab}");
    }
}

#[test]
fn the_same_offer_stating_two_values_is_ambiguous() {
    let fact = confirmed(QUOTE, no_ages());
    let both = original().replace(
        "<p class=\"desc\">학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>",
        "<p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><p>학생 인증 후 15% 할인 혜택을 제공해 드립니다.</p>",
    );
    let resolution = resolve_on(&fact, &both);
    assert!(!resolution.confirms(), "{resolution:?}");
    // Far apart (outside the neighbourhood window) it is the identity's
    // contradiction rule that hides it.
    let far = original().replace(
        "<p>멤버십 회원은 매월 2,000원 적립</p>",
        &format!("<p>멤버십 회원은 매월 2,000원 적립</p>{}", "<p>안내 문구입니다 안내 문구입니다 안내 문구입니다.</p>".repeat(12)),
    ).replace(
        "<section class=\"other\">",
        "<section class=\"student-extra\"><h2 class=\"title\">학생 할인</h2><p>학생 인증 후 15% 할인 혜택을 제공해 드립니다.</p></section><section class=\"other\">",
    );
    assert!(!resolve_on(&fact, &far).confirms());
}

#[test]
fn a_removed_offer_or_a_dropped_neighbour_is_never_confirmed() {
    let fact = confirmed(QUOTE, no_ages());
    let gone = shell("<section><h2>멤버십</h2><p>멤버십 회원은 매월 2,000원 적립</p></section>");
    assert_hidden_on_evidence(&resolve_on(&fact, &gone));
    let dropped = original().replace(
        "<p class=\"note\">30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p>",
        "",
    );
    assert_hidden_on_evidence(&resolve_on(&fact, &dropped));
}

// ── context: ending, exclusion, negation, past dates ──────────────────────

/// Round 3 HIGH: endings after or before the quote, Korean and English.
#[test]
fn ending_or_negation_markers_around_the_quote_never_confirm() {
    let fact = confirmed(QUOTE, no_ages());
    let after = [
        "해당 이벤트는 종료되었습니다.",
        "(종료)",
        "(마감)",
        "온라인 구매는 제외됩니다.",
        "이 혜택은 더 이상 제공되지 않습니다.",
        "신규 가입자는 대상이 아닙니다.",
        "현재 신청 불가",
        "일부 매장에는 적용되지 않아요.",
        "할인 안 됩니다.",
        "대상이 아니에요.",
        "혜택이 없어졌어요.",
        "중복으로 받을 수 없으며",
        "행사 끝!",
        "온라인에서는 못 합니다.",
        "준비 중입니다.",
        "일시 중단",
        "조기 마감되었습니다.",
        "접수 마감",
        "판매 종료",
        "no longer available",
        "Not available for students",
        "Excludes sale items",
        "Except gift cards",
        "Sold out",
        "This promotion is over",
        "Offer terminated",
        "No longer offered",
        "Excluding sale items",
        "Students are not eligible",
        "(~2020.12.31까지)",
        "2021년 3월 31일까지",
        "until 2020-06-30",
    ];
    for marker in after {
        let page = original().replace(QUOTE, &format!("{QUOTE} {marker}"));
        let resolution = resolve_on(&fact, &page);
        assert!(!resolution.confirms(), "after {marker}: {resolution:?}");
    }
    for marker in [
        "해당 이벤트는 종료되었습니다.",
        "This event has ended.",
        "[종료]",
    ] {
        let page = original().replace(QUOTE, &format!("{marker} {QUOTE}"));
        assert!(!resolve_on(&fact, &page).confirms(), "before {marker}");
        // A separate block right before the statement counts too.
        let block = original().replace(
            "<p class=\"desc\">",
            &format!("<p class=\"notice\">{marker}</p><p class=\"desc\">"),
        );
        assert!(
            !resolve_on(&fact, &block).confirms(),
            "sibling before {marker}"
        );
    }
}

#[test]
fn a_future_end_date_keeps_the_offer_and_an_ending_anywhere_hides_it() {
    let today = super::super::health::kst_today();
    assert!(!has_past_end_date("(~2099.12.31까지)", today));
    assert!(has_past_end_date("(~2020.12.31까지)", today));
    // An unchanged page with a future end date confirms; a confirmed fact
    // is not hidden by another event's ending in another section, only by
    // one that names it.
    let future = original().replace(
        "<p>멤버십 회원은 매월 2,000원 적립</p>",
        "<p>멤버십 회원은 매월 2,000원 적립</p><p>적립 기간: ~2099.12.31까지</p>",
    );
    let fact = confirm_on(&future, QUOTE, no_ages()).unwrap();
    assert!(resolve_on(&fact, &future).confirms());
    let ended = original().replace(
        "<p>멤버십 회원은 매월 2,000원 적립</p>",
        "<p>멤버십 회원은 매월 2,000원 적립</p><p>지난 여름 이벤트는 종료되었습니다.</p>",
    );
    let fact = confirm_on(&ended, QUOTE, no_ages()).unwrap();
    assert!(resolve_on(&fact, &ended).confirms());
    let named = original().replace(
        "<p>멤버십 회원은 매월 2,000원 적립</p>",
        "<p>멤버십 회원은 매월 2,000원 적립</p><p>학생 할인 이벤트는 종료되었습니다.</p>",
    );
    let fact = confirm_on(&named, QUOTE, no_ages()).unwrap();
    assert_eq!(
        resolve_on(&fact, &named),
        Resolution::Ambiguous("the page says an offer ended or is not on yet".into())
    );
}

#[path = "resilience_corpus.rs"]
mod corpus;

#[path = "resilience_layout_corpus.rs"]
mod layout_corpus;

#[path = "resilience_false_hide_corpus.rs"]
mod false_hide_corpus;

#[path = "resilience_real_pages.rs"]
mod real_pages;

#[path = "resilience_page_changed_corpus.rs"]
mod page_changed_corpus;

#[path = "resilience_false_ending_corpus.rs"]
mod false_ending_corpus;

#[path = "resilience_adversarial_tests.rs"]
mod adversarial;

/// Replaces the stored notes digest with the after-page's, so the guard
/// under test is the only thing that can decide (the notes digest catches
/// these pages first in normal operation; the guards are defence in depth).
fn with_notes_of(fact: &mut Confirmed, after: &str, quote: &str) {
    let page = view(after);
    let needle = fold_str(quote);
    let notes = holders(&page, &needle)
        .first()
        .map(|&i| page.notes(i).to_string())
        .unwrap_or_default();
    for identity in &mut fact.anchors.fingerprint.as_mut().unwrap().identities {
        identity.notes = notes.clone();
    }
}

#[test]
fn a_changed_holder_count_hides_the_fact() {
    let quote = "월 3,000원 할인";
    let before = tiers(
        "학생 요금제",
        "<p>월 3,000원 할인</p><p>월 3,000원 할인</p>",
    );
    let after = tiers("학생 요금제", "<p>월 3,000원 할인</p>");
    let mut fact = confirm_on(&before, quote, no_ages()).unwrap();
    assert_eq!(
        fact.anchors.fingerprint.as_ref().unwrap().identities[0].holders,
        2
    );
    with_notes_of(&mut fact, &after, quote);
    // The occurrence-counting window would also catch it; take it out too.
    fact.baseline = confirm_on(&after, quote, no_ages()).unwrap().baseline;
    let resolution = resolve_on(&fact, &after);
    assert!(!resolution.confirms(), "{resolution:?}");
}

#[test]
fn a_place_shared_by_several_statements_never_gets_a_proposal() {
    let quote = "월 3,000원 할인";
    let before = tiers(
        "학생 요금제",
        "<p>월 3,000원 할인</p><p>월 5,000원 할인</p>",
    );
    let after = tiers("학생 요금제", "<p>월 5,000원 할인</p>");
    let mut fact = confirm_on(&before, quote, no_ages()).unwrap();
    assert!(!fact.anchors.fingerprint.as_ref().unwrap().identities[0].distinct);
    with_notes_of(&mut fact, &after, "월 5,000원 할인");
    let resolution = resolve_on(&fact, &after);
    assert!(
        !matches!(resolution, Resolution::Proposed(_)),
        "{resolution:?}"
    );
}

#[test]
fn a_value_change_with_a_formatting_change_is_not_attributed() {
    let quote = "구매 금액의 5% 적립";
    let before = tiers("학생 등급", "<h3>1단계</h3><p>구매 금액의 5% 적립</p>");
    let fact = confirm_on(&before, quote, no_ages()).unwrap();
    for after in [
        "<h3>1단계</h3><p>구매 금액의 7% 적립!</p>",
        "<h3>1단계</h3><p>구매금액의 7% 적립</p>",
    ] {
        assert_hidden_on_evidence(&resolve_on(&fact, &tiers("학생 등급", after)));
        assert_eq!(
            shadow_on(&fact, &tiers("학생 등급", after)),
            None,
            "{after}"
        );
    }
    // Only the number changed: read in shadow.
    let after = tiers("학생 등급", "<h3>1단계</h3><p>구매 금액의 7% 적립</p>");
    assert_hidden_on_evidence(&resolve_on(&fact, &after));
    assert!(shadow_on(&fact, &after).is_some());
}

// The reviewer's round-5 module, kept verbatim as the gate.
#[allow(clippy::useless_format)]
#[rustfmt::skip]
#[path = "resilience_adversarial_r5_tests.rs"]
mod adversarial_r5;

// The reviewer's round-6 module, kept verbatim. It prints verdicts without
// asserting; `r6_gate` runs it and asserts.
#[rustfmt::skip]
#[path = "resilience_adversarial_r6_tests.rs"]
mod adversarial_r6;

// The reviewer's round-7 module, kept verbatim. It prints verdicts without
// asserting; `r7_gate` runs it and asserts.
#[rustfmt::skip]
#[path = "resilience_adversarial_r7_tests.rs"]
mod adversarial_r7;

// The reviewer's round-8 cases (Part 1c and 2), unchanged, without the
// publish checks that need the fresh-extraction modules.
#[rustfmt::skip]
#[allow(unused_imports, dead_code)]
#[path = "resilience_r8_replay_tests.rs"]
mod adversarial_r8;

// The reviewer's round-8 module, verbatim, publish checks included (the
// publish and fresh-extraction modules are on this branch since the
// bootstrap merge). `r8w_gate` asserts its W cases.
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "resilience_adversarial_r8_tests.rs"]
mod adversarial_r8_full;

/// The round-8 publish checks: W1–W5 refused, the positive control
/// published, the cap-change control refused.
#[test]
fn r8w_gate() {
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            &format!("{module}::adversarial_r8_full::r8_part2_publish_field_checks"),
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("child test run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("R8W W0p Published(expected)"), "{stdout}");
    assert!(
        stdout.contains("R8W TOTAL cases=7 wrong_updates=0"),
        "{stdout}"
    );
    assert!(!stdout.contains("WrongUpdate"), "{stdout}");
}

/// Fails on any false confirm or wrong update among the round-8 Part 2
/// verdicts, except the accepted limits.
#[test]
fn r8_gate() {
    let verdicts = child_verdicts("R8 ", "adversarial_r8::r8_part2_adversarial");
    assert!(verdicts.len() >= 46, "{verdicts:?}");
    let ids_of = |wanted: &[&str]| -> Vec<&str> {
        verdicts
            .iter()
            .filter(|(_, verdict)| wanted.contains(&verdict.as_str()))
            .map(|(id, _)| id.as_str())
            .collect()
    };
    assert_false_confirm_floor("r8", &ids_of(&["FalseConfirm"]), &[]);
    let bad = ids_of(&["WrongUpdate", "SeedFailed"]);
    assert!(bad.is_empty(), "{bad:#?}");
}

// The reviewer's round-9 module, kept verbatim. Part T and Part R print;
// `r9_gate` runs them and asserts. Its DB test asserts on its own.
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "resilience_r9_review_tests.rs"]
mod r9_review_probes;

/// Fails when the default `hide_only` allowlist hides any of the round-9
/// active probes (E1–E14, R1–R2, D1–D16, S1–S7, C3), or any real page on
/// any fragment, now or read 30 and 75 days later.
#[test]
fn r9_gate() {
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let run = |test: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                &format!("{module}::r9_review_probes::{test}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{test} failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let probes = run("r9_part_t_default_reason_precision_on_active_offers");
    let lines: Vec<&str> = probes
        .lines()
        .filter_map(|l| l.find("R9P ").map(|at| &l[at + 4..]))
        .filter(|l| !l.starts_with("TOTAL"))
        .collect();
    assert_eq!(lines.len(), 43, "{probes}");
    let hidden: Vec<&&str> = lines
        .iter()
        .filter(|l| {
            let id = l.split_whitespace().next().unwrap_or("");
            let active = !id.starts_with('C') || id == "C3";
            active && l.contains("hide_only_hides=true")
        })
        .collect();
    assert!(hidden.is_empty(), "{hidden:#?}");
    // The ended controls C1 and C2 are never shown.
    let shown_ended: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("C1 ") || l.starts_with("C2 "))
        .filter(|l| !l.contains(" HIDDEN_live "))
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_false_confirm_floor("r9", &shown_ended, &[]);
    let pages = run("r9_part_r_real_pages_rerun_and_vacuity");
    assert!(pages.contains("hide_only_any_fragment=0 "), "{pages}");
    assert!(
        pages.contains("R9RP SHIFTED trusted hides when read later: []"),
        "{pages}"
    );
}

// The reviewer's re-review module of 31536232, kept verbatim. It prints;
// `r9b_gate` runs it and asserts.
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "resilience_r9b_review_tests.rs"]
mod r9b_review_probes;

/// The re-review gate: the default `hide_only` allowlist hides none of the
/// active probes N1–N11; on the database, in shadow and in hide_only, a
/// confirming page is never held over 30 days and an ended (one signal),
/// removed or unreadable one is held by day 4.
#[test]
fn r9b_gate() {
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let run = |filter: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                &format!("{module}::r9b_review_probes::{filter}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{filter} failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let probes = run("r9b_part_n_two_signal_and_structured");
    let lines: Vec<&str> = probes
        .lines()
        .filter_map(|l| l.find("R9B ").map(|at| &l[at + 4..]))
        .collect();
    assert_eq!(lines.len(), 14, "{probes}");
    // The ended controls N12, C1 and C2 are never shown.
    let shown_ended: Vec<&str> = lines
        .iter()
        .filter(|l| ["N12 ", "C1 ", "C2 "].iter().any(|id| l.starts_with(id)))
        .filter(|l| !l.contains(" HIDDEN_live "))
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_false_confirm_floor("r9b", &shown_ended, &[]);
    let hidden: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with('N') && !l.starts_with("N12 "))
        .filter(|l| l.contains("hide_only_hides=true"))
        .collect();
    assert!(hidden.is_empty(), "{hidden:#?}");
    // Part D: N1–N11 are no trusted reason at all (shadow metrics included);
    // the ended control N12 still is.
    let trusted: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with('N') && !l.starts_with("N12 "))
        .filter(|l| {
            l.contains("key=section_ended_confirmed ") || l.contains("key=structured_ended ")
        })
        .collect();
    assert!(trusted.is_empty(), "{trusted:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("N12 ") && l.contains("key=section_ended_confirmed ")),
        "{probes}"
    );
    if std::env::var("TEST_DATABASE_URL").is_err() {
        eprintln!("TEST_DATABASE_URL unset; database part skipped");
        return;
    }
    let db = run("r9b_db::");
    let held_on = |name: &str| -> Option<u32> {
        db.lines()
            .filter_map(|l| l.find("R9BDB ").map(|at| &l[at + 6..]))
            .filter(|l| l.starts_with(&format!("{name} day")) && l.contains("active=false"))
            .find_map(|l| l[name.len() + 4..].split(':').next()?.parse().ok())
    };
    let end_active = |name: &str| db.contains(&format!("R9BDB {name} END active=true"));
    for mode in ["shadow", "hide_only"] {
        let good = format!("{mode}/good");
        assert!(
            end_active(&good) && held_on(&good).is_none(),
            "{good}\n{db}"
        );
        for case in ["ended_one_signal", "offer_removed", "http_500"] {
            let name = format!("{mode}/{case}");
            let day = held_on(&name);
            assert!(
                day.is_some_and(|d| d <= 4) && !end_active(&name),
                "{name}: {day:?}\n{db}"
            );
        }
    }
}

/// The Part D must-hide corpus (the reviewer's probes of 0b087449): every
/// ended or hidden-quote page fails to confirm and every ended clause reads
/// as an ending; the publish probes refuse every crossed binding (B0 the
/// control publishes); an age next to a percent range is refused.
#[test]
fn partd_gate() {
    let run = |test: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{test} failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let lines = |out: &str, prefix: &str| -> Vec<String> {
        out.lines()
            .filter_map(|l| l.find(prefix).map(|at| l[at..].to_string()))
            .collect()
    };
    let pages = run("corporate_auto::resilience::probe_partd::probe_pages");
    let page_lines = lines(&pages, "PAGE ");
    assert_eq!(page_lines.len(), 39, "{pages}");
    let confirmed: Vec<&String> = page_lines
        .iter()
        .filter(|l| l.contains("confirms=true"))
        .collect();
    assert!(confirmed.is_empty(), "{confirmed:#?}");
    assert!(
        pages.contains("DIGEST panel_change_changes_digest=true"),
        "{pages}"
    );
    let clauses = run("corporate_auto::resilience::probe_partd::probe_clauses");
    let clause_lines = lines(&clauses, "CLAUSE ");
    assert_eq!(clause_lines.len(), 38, "{clauses}");
    let missed: Vec<&String> = clause_lines
        .iter()
        .filter(|l| l.ends_with("=> None"))
        .collect();
    assert!(missed.is_empty(), "{missed:#?}");
    let ages = run("corporate_auto::resilience::probe_partd::probe_ages");
    for refused in [
        "AGE \"10~20% 할인 (19~24세)\"",
        "AGE \"19세~24세 미만 10% 할인\"",
    ] {
        let line = lines(&ages, "AGE ")
            .into_iter()
            .find(|l| l.starts_with(refused))
            .unwrap_or_default();
        assert!(line.contains("agree=false"), "{refused}: {line}");
    }
    let r2 = run("corporate_auto::resilience::probe_r2::probe_r2_pages");
    let r2_pages = lines(&r2, "R2PAGE ");
    assert_eq!(r2_pages.len(), 32, "{r2}");
    let confirmed: Vec<&String> = r2_pages
        .iter()
        .filter(|l| l.contains("confirms=true"))
        .collect();
    assert!(confirmed.is_empty(), "{confirmed:#?}");
    let r2_clauses = run("corporate_auto::resilience::probe_r2::probe_r2_clauses");
    let r2_clause_lines = lines(&r2_clauses, "R2CLAUSE ");
    assert_eq!(r2_clause_lines.len(), 21, "{r2_clauses}");
    let missed: Vec<&String> = r2_clause_lines
        .iter()
        .filter(|l| l.ends_with("=> None"))
        .collect();
    assert!(missed.is_empty(), "{missed:#?}");
    let r2_bound = run("corporate_auto::publish::probe_r2_pub::probe_r2_bound");
    for line in lines(&r2_bound, "R2BOUND ") {
        assert!(line.contains("=> Err("), "{line}");
    }
    let agree = lines(&r2_bound, "R2AGREE ");
    assert_eq!(agree.len(), 4, "{r2_bound}");
    assert!(agree.iter().all(|l| l.ends_with("=> false")), "{agree:#?}");
    let r3 = run("corporate_auto::publish::probe_r3_pub::probe_r3_bound");
    let r3_bound = lines(&r3, "R3BOUND ");
    assert_eq!(r3_bound.len(), 21, "{r3}");
    for line in &r3_bound {
        if line.starts_with("R3BOUND C0_control") {
            assert!(line.ends_with("=> Ok(())"), "{line}");
        } else {
            assert!(line.contains("=> Err("), "{line}");
        }
    }
    let r3_agree = lines(&r3, "R3AGREE ");
    assert_eq!(r3_agree.len(), 7, "{r3}");
    assert!(
        r3_agree.iter().all(|l| l.ends_with("=> false")),
        "{r3_agree:#?}"
    );
    let r4 = run("corporate_auto::publish::probe_r4_pub::probe_r4_bound");
    let r4_bound = lines(&r4, "R4BOUND ");
    assert_eq!(r4_bound.len(), 21, "{r4}");
    for line in &r4_bound {
        if line.starts_with("R4BOUND D0_control") {
            assert!(line.ends_with("=> Ok(())"), "{line}");
        } else {
            assert!(line.contains("=> Err("), "{line}");
        }
    }
    let bound = run("corporate_auto::publish::probe_pub::probe_bound");
    for line in lines(&bound, "BOUND ") {
        if line.starts_with("BOUND B0_control") {
            assert!(line.ends_with("=> Ok(())"), "{line}");
        } else {
            assert!(line.contains("=> Err("), "{line}");
        }
    }
}

/// The false-confirm floor of the replay corpora (r5–r9 and the generated
/// corpus): no offer the page no longer makes is shown, beyond the limits
/// accepted by name ([`R6_ACCEPTED`], [`R7_ACCEPTED`]). A reader change may
/// lower a count, never raise it.
pub(super) const FALSE_CONFIRM_FLOOR: usize = 0;

/// Fails when a gate's false confirms, less its accepted limits, exceed
/// [`FALSE_CONFIRM_FLOOR`].
fn assert_false_confirm_floor(gate: &str, false_confirms: &[&str], accepted: &[&str]) {
    let over: Vec<&&str> = false_confirms
        .iter()
        .filter(|id| !accepted.contains(id))
        .collect();
    // The floor is a named limit that is 0 today; clippy reads `<= 0` as absurd.
    #[allow(clippy::absurd_extreme_comparisons)]
    let within = over.len() <= FALSE_CONFIRM_FLOOR;
    assert!(
        within,
        "{gate}: false confirms over the floor of {FALSE_CONFIRM_FLOOR}: {over:#?}"
    );
}

/// Verdict lines (`R7 <id> <verdict> | …`) of one reviewer test, run in a
/// child process of this test binary.
fn child_verdicts(prefix: &str, test: &str) -> Vec<(String, String)> {
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            &format!("{module}::{test}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("child test run");
    assert!(output.status.success(), "{test} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.find(prefix).map(|at| &l[at + prefix.len()..]))
        .filter(|l| !l.starts_with("TOTAL"))
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            Some((words.next()?.to_string(), words.next()?.to_string()))
        })
        .collect()
}

/// Accepted limits of the round-7 module (handoff §17): F6, client state in
/// hydration data the server-rendered page does not show.
const R7_ACCEPTED: &[&str] = &["F6"];

/// Fails on any false confirm or wrong update among the round-7 module's 85
/// verdicts, except the accepted limits.
#[test]
fn r7_gate() {
    let verdicts = child_verdicts("R7 ", "adversarial_r7::r7_adversarial_replay");
    assert_eq!(verdicts.len(), 85, "{verdicts:?}");
    let ids_of = |wanted: &[&str]| -> Vec<&str> {
        verdicts
            .iter()
            .filter(|(_, verdict)| wanted.contains(&verdict.as_str()))
            .map(|(id, _)| id.as_str())
            .collect()
    };
    assert_false_confirm_floor("r7", &ids_of(&["FalseConfirm"]), R7_ACCEPTED);
    let bad = ids_of(&["WrongUpdate", "SeedFailed"]);
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The accepted limit of the round-6 module (handoff §16): A9, an image
/// swapped with its alt text unchanged; the page's text did not change.
const R6_ACCEPTED: &[&str] = &["A9"];

/// Runs the round-6 module in a child process of this test binary and
/// fails on any false confirm or wrong update among its 40 verdicts, or a
/// dependent follow on a changed page. A9 (an image swapped, alt text
/// unchanged) is the one accepted miss: the page's text did not change, and
/// image-only changes are out of scope (handoff §16).
#[test]
fn r6_gate() {
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let run = |test: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                &format!("{module}::adversarial_r6::{test}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{test} failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let replay = run("r6_adversarial_replay");
    // A line may start with the test harness's own `test … ...` prefix.
    let verdicts: Vec<&str> = replay
        .lines()
        .filter_map(|l| l.find("R6 ").map(|at| &l[at..]))
        .filter(|l| !l.starts_with("R6 TOTAL"))
        .collect();
    assert_eq!(verdicts.len(), 40, "{replay}");
    let ids_of = |wanted: &str| -> Vec<&str> {
        verdicts
            .iter()
            .filter_map(|l| {
                let mut words = l.split_whitespace().skip(1);
                let (id, verdict) = (words.next()?, words.next()?);
                (verdict == wanted).then_some(id)
            })
            .collect()
    };
    assert_false_confirm_floor("r6", &ids_of("FalseConfirm"), R6_ACCEPTED);
    let bad = ids_of("WrongUpdate");
    assert!(bad.is_empty(), "{bad:#?}");
    let dependent = run("r6_dependent_follow_across_sections");
    assert!(
        dependent.contains("-> Hidden") && !dependent.contains("FalseConfirm"),
        "{dependent}"
    );
}

fn identity_of_quote(html: &str, quote: &str) -> Identity {
    capture(&view(html), quote).expect("holder").identities[0].clone()
}

#[test]
fn the_table_header_grid_honours_colspan_and_rowspan() {
    let table = "<section><h2>등급별 할인율</h2><table><thead>\
         <tr><th rowspan=\"2\">등급</th><th colspan=\"2\">할인율</th></tr>\
         <tr><th>온라인</th><th>매장</th></tr></thead><tbody>\
         <tr><th>VIP</th><td>10% 할인</td><td>5% 할인</td></tr></tbody></table></section>";
    assert_eq!(
        identity_of_quote(&shell(table), "5% 할인").labels,
        ["vip", "할인율", "매장"]
    );
    assert_eq!(
        identity_of_quote(&shell(table), "10% 할인").labels,
        ["vip", "할인율", "온라인"]
    );
    let rows =
        "<section><h2>등급별 적립</h2><table><tr><th>등급</th><th>구분</th><th>적립률</th></tr>\
         <tr><th rowspan=\"2\">VIP</th><td>온라인</td><td>10% 적립</td></tr>\
         <tr><td>매장</td><td>7% 적립</td></tr></table></section>";
    assert_eq!(
        identity_of_quote(&shell(rows), "7% 적립").labels,
        ["vip", "매장", "적립률"]
    );
}

#[test]
fn a_tier_name_in_the_card_head_labels_the_card_body() {
    let cards = "<section><h2>멤버십 등급 혜택</h2><div class=\"cards\">\
         <div class=\"card\"><div class=\"card-head\"><span>VIP</span></div><div class=\"card-body\"><p class=\"rate\">결제 금액의 5% 적립</p></div></div>\
         <div class=\"card\"><div class=\"card-head\"><span>일반</span></div><div class=\"card-body\"><p class=\"rate\">결제 금액의 1% 적립</p></div></div></div></section>";
    assert_eq!(
        identity_of_quote(&shell(cards), "결제 금액의 5% 적립").labels,
        ["vip"]
    );
}

#[test]
fn a_range_end_without_a_year_takes_the_start_year() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
    // Without the start year, 3.31 would be read as next March.
    assert!(has_past_end_date("이벤트 기간: 2025.11.01 ~ 3.31", today));
    assert!(has_past_end_date("2026.09.01 ~ 10.13", today));
    assert!(has_past_end_date("9.1 ~ 10.13", today));
    assert!(has_past_end_date("2026년 9월 1일 ~ 10월 13일", today));
    assert!(has_past_end_date("~ 10월 13일", today));
    assert!(!has_past_end_date("2026.09.01 ~ 12.31", today));
    assert!(
        !has_past_end_date("2026.12.01 ~ 1.15", today),
        "January after a December start is next year"
    );
    assert!(
        !has_past_end_date("3.5~4.5% 적립", today),
        "a percentage range is no date"
    );
}

#[test]
fn the_surroundings_digest_alone_reflects_a_far_value_change() {
    let page = |cap: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p class=\"offer\">{QUOTE}</p><p class=\"about\">본 혜택은 제휴사 사정에 따라 변경될 수 있으며 자세한 내용은 고객센터로 문의해 주시기 바랍니다. 본 혜택은 제휴사 사정에 따라 변경될 수 있습니다.</p><p class=\"cap\">할인 한도: 최대 {cap}</p></section>"
        ))
    };
    let before = identity_of_quote(&page("5,000원"), QUOTE);
    let after = identity_of_quote(&page("50,000원"), QUOTE);
    assert_ne!(before.notes, after.notes);
    assert_eq!(before.wording, after.wording, "only a value changed");
}

// ── round 7: the whole page ───────────────────────────────────────────────

fn digest_of(html: &str) -> String {
    view(html).page_digest().to_string()
}

#[test]
fn the_page_digest_ignores_markup_and_reads_every_text() {
    let base = shell(
        "<section><h2>학생 할인</h2><p>학생 인증 시 월 3,000원 할인</p><p>※ 1인 1회</p></section>",
    );
    // Markup, classes, whitespace, comments, width, a thousands comma.
    for same in [
        shell("<div class=\"x\"><h3>학생 할인</h3><div><span>학생 인증 시</span> <b>월 3,000원</b> 할인</div><div>※ 1인 1회</div></div>"),
        shell("<section>\n  <h2> 학생   할인 </h2>\n  <p>학생 인증 시 월 3<!-- -->,000<!-- -->원 할인</p>\n<p>※ 1인 1회</p></section>"),
        shell("<section><h2>학생 할인</h2><p>학생 인증 시 월 ３０００원 할인</p><p>※ 1인 1회</p></section>"),
    ] {
        assert_eq!(digest_of(&base), digest_of(&same), "{same}");
    }
    // Any text anywhere: a body-level bar, a modal, a footnote mark, an
    // image's alt text, text moved into or out of view, a tab label.
    for changed in [
        format!("<div class=\"top\">공지</div>{base}"),
        base.replace(
            "</body>",
            "<div class=\"modal\" role=\"dialog\"><p>유의사항</p></div></body>",
        ),
        base.replace("※ 1인 1회", "※※ 1인 1회"),
        base.replace(
            "<p>※ 1인 1회</p>",
            "<p>※ 1인 1회</p><img src=\"a.png\" alt=\"이벤트\">",
        ),
        base.replace("<p>※ 1인 1회</p>", "<p hidden>※ 1인 1회</p>"),
        base.replace(
            "학생 할인</h2>",
            "학생 할인</h2><div role=\"tablist\"><button>신입생</button></div>",
        ),
        base.replace("3,000원", "3,500원"),
        base.replace("월 3,000원", "월 3 000원"),
    ] {
        assert_ne!(digest_of(&base), digest_of(&changed), "{changed}");
    }
    // Hidden text is page text too (a closed modal, a collapsed note).
    let modal = |text: &str| {
        base.replace(
            "</body>",
            &format!("<div class=\"modal\" aria-hidden=\"true\"><p>{text}</p></div></body>"),
        )
    };
    assert_ne!(
        digest_of(&modal("※ 온라인 결제 시 적용")),
        digest_of(&modal("※ 매장 결제 시 적용"))
    );
    assert!(!adopted_on(
        &modal("※ 이벤트가 종료되었습니다"),
        "학생 인증 시 월 3,000원 할인"
    )
    .confirms());
    // No length cap: a change after 300,000 characters still counts.
    let long = |tail: &str| shell(&format!("<p>{}</p><p>{tail}</p>", "가".repeat(400_000)));
    assert_ne!(digest_of(&long("하나")), digest_of(&long("둘")));
}

#[test]
fn every_end_date_form_on_the_page_is_read() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
    for ended in [
        "26.09.01 ~ 26.10.13",
        "2026. 10. 13.(화)까지",
        "2026.10.13.까지",
        "2026년 10월 13일(화) 18시까지",
        "10월 13일 자정까지",
        "2026.09.01 \u{223C} 2026.10.13",
        "2026.09.01 \u{301C} 2026.10.13",
        "Sep 1 – Oct 13, 2026",
        "until October 13, 2026",
        "~ 10월 13일",
    ] {
        assert!(
            has_past_end_date(ended, today) || has_past_end_date(&fold_str(ended), today),
            "{ended}"
        );
    }
    for open in [
        "2026.09.01 ~ 2026.12.31",
        "Sep 1 – Dec 31, 2026",
        "~ 12월 31일까지",
    ] {
        assert!(!has_past_end_date(open, today), "{open}");
        assert!(!has_past_end_date(&fold_str(open), today), "{open}");
    }
    for ahead in [
        "변경된 할인율은 2026년 11월 1일부터 적용됩니다.",
        "2026.11.01 ~ 2026.12.31",
        "effective Nov 1, 2026",
    ] {
        assert!(has_future_start(ahead, today), "{ahead}");
    }
    assert!(!has_future_start("2026.09.01 ~ 2026.12.31", today));
    assert!(!has_future_start("2026년 9월 1일부터 적용", today));
}

fn adopted_on(html: &str, quote: &str) -> Resolution {
    resolve(
        &view(html),
        &StoredFact {
            quote,
            baseline: None,
            ages: no_ages(),
            adoption_open: true,
            anchors: &Anchors::default(),
        },
    )
}

#[test]
fn an_ending_or_a_passed_date_anywhere_on_an_unchanged_page_hides_the_fact() {
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p></section><section><h2>다른 소식</h2><p>{extra}</p></section>"
        ))
    };
    assert!(adopted_on(&page("매월 새 소식을 전해 드립니다."), QUOTE).confirms());
    for extra in [
        "가을 이벤트 기간: 2026.09.01 ~ 2026.10.13",
        "본 이벤트는 종료되었습니다.",
        "This promotion has ended.",
        "SOLD OUT",
        "2026년 12월 1일부터 시행됩니다.",
    ] {
        let resolution = adopted_on(&page(extra), QUOTE);
        assert!(!resolution.confirms(), "{extra}: {resolution:?}");
    }
    // A confirmed fact on an unchanged page reads its own dates every run:
    // its own period passing since the last confirmation hides it, another
    // section's period does not (that text was judged at confirmation).
    let own = shell(&format!(
        "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>이벤트 기간: 2026.09.01 ~ 2099.12.31</p></section>\
         <section><h2>다른 소식</h2><p>매월 새 소식을 전해 드립니다.</p></section>"
    ));
    let other = page("가을 이벤트 기간: 2026.09.01 ~ 2099.12.31");
    let later = |html: &str| {
        let fact = confirm_on(html, QUOTE, no_ages()).unwrap();
        assert!(resolve_on(&fact, html).confirms());
        let mut page = view(html);
        page.today = chrono::NaiveDate::from_ymd_opt(2100, 1, 1).unwrap();
        resolve_in(&fact, &page)
    };
    assert_eq!(later(&own), Resolution::Ambiguous(SECTION_DATE_PAST.into()));
    assert!(later(&other).confirms());
}

/// Text is judged once: a fact confirmed before is not re-judged by ending
/// words or exclusions in its unchanged section, only by its own dates,
/// its two-signal end, structured data and, on a page with several offers,
/// a finished ending elsewhere that names the offer.
#[test]
fn a_confirmed_fact_is_not_rejudged_on_text_already_judged() {
    let page = |note: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><ul class=\"faq\"><li>{note}</li></ul></section>"
        ))
    };
    for note in [
        "Q. 졸업 후에도 혜택을 받을 수 있나요? 졸업생은 신청 불가합니다.",
        "Q. I graduated. A. Your account is no longer eligible.",
        "휴학생 및 졸업생은 학생 할인 대상에서 제외됩니다.",
        "인터넷 익스플로러 지원이 종료되었습니다.",
    ] {
        let html = page(note);
        // Read the first time, the note keeps the offer from adoption.
        assert!(!adopted_on(&html, QUOTE).confirms(), "{note}");
        // Confirmed before (a stored baseline, its section captured), it
        // is not re-judged on the same text.
        let fact = Confirmed {
            quote: QUOTE.to_string(),
            baseline: PageEvidence::from_html(&html)
                .window_signature(QUOTE)
                .unwrap(),
            anchors: Anchors {
                fingerprint: capture(&view(&html), QUOTE),
                ..Anchors::default()
            },
            ages: no_ages(),
        };
        assert!(resolve_on(&fact, &html).confirms(), "{note}");
        // A legacy fact (no stored anchors) is not read as on first sight.
        let legacy = Confirmed {
            anchors: Anchors::default(),
            ..fact
        };
        assert!(resolve_on(&legacy, &html).confirms(), "legacy: {note}");
    }
    // On a page with several offers, a finished ending elsewhere that names
    // the offer still hides a confirmed fact; an unnamed one does not.
    let several = |elsewhere: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p></section>\
             <section><h2>멤버십</h2><p>멤버십 회원은 매월 2,000원 적립</p><p>{elsewhere}</p></section>"
        ))
    };
    let base = several("매월 새 소식을 전해 드립니다.");
    let fact = confirm_on(&base, QUOTE, no_ages()).unwrap();
    let legacy = Confirmed {
        anchors: Anchors::default(),
        ..fact
    };
    assert_eq!(
        resolve_on(&legacy, &several("학생 할인 이벤트가 종료되었습니다.")),
        Resolution::Ambiguous(PAGE_SAYS_ENDED.into())
    );
    assert!(resolve_on(&legacy, &several("조기 마감되었습니다.")).confirms());
}

#[test]
fn structured_data_with_a_passed_end_or_a_coming_start_hides_the_fact() {
    let page = |json: &str| {
        shell(&format!(
            "<script type=\"application/ld+json\">{json}</script><section><h2>학생 할인</h2><p>{QUOTE}</p></section>"
        ))
    };
    assert!(adopted_on(
        &page(r#"{"@type":"Offer","validThrough":"2099-12-31"}"#),
        QUOTE
    )
    .confirms());
    for json in [
        r#"{"@type":"Offer","validThrough":"2020-10-13"}"#,
        r#"{"@type":"Event","endDate":"2020-10-13T23:59"}"#,
        r#"{"@type":"Offer","validFrom":"2099-01-01"}"#,
    ] {
        assert!(!adopted_on(&page(json), QUOTE).confirms(), "{json}");
    }
}

#[test]
fn a_fact_confirmed_before_pages_were_digested_is_adopted_again_or_hidden() {
    let mut fact = confirmed(QUOTE, no_ages());
    fact.anchors.fingerprint.as_mut().unwrap().page.clear();
    // The adoption window is open: adopted again on this page.
    let html = original();
    let resolution = resolve(
        &view(&html),
        &StoredFact {
            quote: &fact.quote,
            baseline: Some(&fact.baseline),
            ages: fact.ages,
            adoption_open: true,
            anchors: &fact.anchors,
        },
    );
    // Adopted again only while its value window still reads as its stored
    // baseline; the baseline is kept (no new adoption).
    assert!(
        matches!(
            resolution,
            Resolution::Exact(FactOutcome::Confirmed { adopted: false, .. })
        ),
        "{resolution:?}"
    );
    let changed = html.replace("매월 2,000원", "매월 3,000원");
    let resolution = resolve(
        &view(&changed),
        &StoredFact {
            quote: &fact.quote,
            baseline: Some(&fact.baseline),
            ages: fact.ages,
            adoption_open: true,
            anchors: &fact.anchors,
        },
    );
    assert!(
        !resolution.confirms(),
        "a changed window is not adopted again: {resolution:?}"
    );
    // Closed: the stored baseline still decides, as for any stored fact.
    assert!(resolve_on(&fact, &html).confirms());
    assert_hidden_on_evidence(&resolve_on(&fact, &changed));
}

#[test]
fn a_quote_spanning_elements_still_binds_the_page() {
    // Every element holding the whole quote is longer than an element is
    // read for (1,000 chars), so no element holds it.
    let filler = "본 혜택은 제휴사 사정에 따라 변경될 수 있습니다. ".repeat(25);
    let html = shell(&format!(
        "<div><p>{filler} 학생 인증 후</p><p>10% 할인 혜택을 제공해 드립니다. {filler}</p></div>"
    ));
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let fact = confirm_on(&html, quote, no_ages()).expect("verbatim across elements");
    let fingerprint = fact
        .anchors
        .fingerprint
        .as_ref()
        .expect("bound to the page");
    assert!(fingerprint.identities.is_empty());
    assert_eq!(fingerprint.page, digest_of(&html));
    assert!(resolve_on(&fact, &html).confirms());
    // A notice added elsewhere changes neither the quote nor its window.
    assert!(resolve_on(&fact, &html.replace("</div>", "<p>공지</p></div>")).confirms());
}

// ── round 7: labelled volatile tokens, conditional endings ────────────────

#[test]
fn labelled_volatile_tokens_are_folded_and_bare_numbers_are_not() {
    let page = |line: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>{line}</p></section>"
        ))
    };
    for (one, two) in [
        ("조회 123", "조회 124"),
        ("조회수: 1,234", "조회수: 2,000"),
        ("5분 전", "17분 전"),
        ("D-3", "D-2"),
        ("등록일 2026.10.01", "등록일 2026.10.15"),
    ] {
        assert_eq!(
            digest_of(&page(one)),
            digest_of(&page(two)),
            "{one} / {two}"
        );
    }
    for (one, two) in [
        ("123", "124"),
        ("친구추천 10,000원", "친구추천 20,000원"),
        ("3일 전 취소 가능", "5일 전 취소 가능"),
        ("조회 기간 3개월", "조회 기간 6개월"),
    ] {
        assert_ne!(
            digest_of(&page(one)),
            digest_of(&page(two)),
            "{one} / {two}"
        );
    }
    // A countdown is folded, but the deadline is still read: a stated end
    // date that has passed hides the fact.
    let resolution = adopted_on(&page("D-3 · 이벤트 기간: 2026.09.01 ~ 2026.10.13"), QUOTE);
    assert_eq!(resolution, Resolution::Ambiguous(SECTION_DATE_PAST.into()));
}

#[test]
fn a_conditional_ending_or_a_link_label_does_not_hide_the_offer() {
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><div class=\"terms\">{extra}</div></section>"
        ))
    };
    for extra in [
        "<p>※ 재고 소진 시 조기 종료될 수 있습니다.</p>",
        "<p>※ 재고 소진 시 조기 종료</p>",
        "<p>부득이한 경우 종료될 수 있습니다.</p>",
        "<p>당사 사정에 따라 변경·종료될 수 있습니다.</p>",
        "<p>마감 임박!</p>",
        "<p>This offer may end without notice.</p>",
        "<p>이벤트 종료 예정일: 2099.12.31</p>",
        "<p>이벤트 종료: 2099.12.31</p>",
        "<div role=\"tablist\"><button role=\"tab\">진행중</button><button role=\"tab\">종료된 이벤트</button></div>",
        "<nav><a href=\"/on\">진행중</a> <a href=\"/off\">종료</a></nav>",
    ] {
        let resolution = adopted_on(&page(extra), QUOTE);
        assert!(resolution.confirms(), "{extra}: {resolution:?}");
    }
}

#[test]
fn a_stated_ending_anywhere_still_hides_the_offer() {
    let page = |top: &str, extra: &str| {
        shell(&format!(
            "{top}<section><h2>학생 할인</h2><p>{QUOTE}</p><div class=\"terms\">{extra}</div></section>"
        ))
    };
    for (top, extra) in [
        ("", "<p>이벤트가 종료되었습니다.</p>"),
        ("", "<p>접수가 마감되었습니다.</p>"),
        ("", "<p>종료된 이벤트입니다.</p>"),
        ("", "<p>재고 소진으로 조기 종료되었습니다.</p>"),
        ("", "<p>재고 소진 시 조기 종료되었습니다.</p>"),
        ("", "<p>(종료)</p>"),
        ("", "<p>판매 종료</p>"),
        ("", "<p>준비 중입니다.</p>"),
        ("", "<p>This promotion has ended.</p>"),
        ("", "<p>SOLD OUT</p>"),
        ("", "<p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>"),
        ("", "<p>이벤트 종료: 2026.10.13</p>"),
        ("", "<p>신청 마감일 2026년 10월 13일</p>"),
        // r6 A1–A5 at first sight: body-level bars, modals, notices.
        ("<div class=\"top-notice\"><p>학생 할인 이벤트가 종료되었습니다.</p></div>", ""),
        ("<div class=\"top-notice\"><p>모든 대학생 대상 혜택 운영을 마쳤습니다. 그동안 성원에 감사드립니다.</p></div>", ""),
        ("", "<div class=\"modal\" role=\"dialog\"><p>※ 이벤트가 종료되었습니다</p></div>"),
        ("<div class=\"notice-box\"><p>※ 학생 할인은 종료되었습니다.</p></div>", ""),
        // A link whose label states the ending of this offer is not a
        // control label once it says so in a sentence outside the link.
        ("", "<p>학생 할인은 종료되었습니다. <a href=\"/new\">새 이벤트 보기</a></p>"),
        // Round 8: 선착순 no longer governs an ending, and a link in the
        // content is read (only navigation and unselected tabs are not).
        ("", "<p>선착순 100명 마감</p>"),
        ("", "<a href=\"/events/past\">지난 이벤트</a>"),
        // A whole offer card made clickable is content, not a label.
        ("", "<a href=\"/card\" class=\"card\"><h3>학생 할인 카드</h3><p>이벤트 종료</p><p>상세 혜택 설명이 이어지는 카드입니다</p></a>"),
    ] {
        let resolution = adopted_on(&page(top, extra), QUOTE);
        assert!(!resolution.confirms(), "{top}{extra}: {resolution:?}");
    }
}

// ── round 8: dates, clauses, controls, regions, folds, structured data ────

fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn clock(today: chrono::NaiveDate, hour: u32, seen: Option<chrono::NaiveDate>) -> Clock {
    Clock {
        today,
        time: chrono::NaiveTime::from_hms_opt(hour, 0, 0).unwrap(),
        seen,
        first_sight: true,
    }
}

#[test]
fn end_dates_are_read_against_the_first_sight_and_the_time_of_day() {
    // (text, first sight, a day still on, a day passed)
    let cases = [
        (
            "이벤트 기간 ~ 3월 31일까지",
            day(2026, 3, 1),
            day(2026, 3, 20),
            day(2026, 10, 18),
        ),
        (
            "이번 달 말까지 진행",
            day(2026, 9, 10),
            day(2026, 9, 30),
            day(2026, 10, 18),
        ),
        (
            "오늘까지만 진행되는 혜택",
            day(2026, 10, 10),
            day(2026, 10, 10),
            day(2026, 10, 11),
        ),
        (
            "이번 주까지 한정",
            day(2026, 10, 14),
            day(2026, 10, 18),
            day(2026, 10, 19),
        ),
        (
            "9월 말까지 진행",
            day(2026, 9, 1),
            day(2026, 9, 30),
            day(2026, 10, 1),
        ),
        (
            "신학기 맞이 9월까지 진행",
            day(2026, 9, 1),
            day(2026, 9, 15),
            day(2026, 10, 18),
        ),
        (
            "9월 한 달간 진행",
            day(2026, 9, 1),
            day(2026, 9, 15),
            day(2026, 10, 18),
        ),
        (
            "2026년 9월 한정 혜택",
            day(2026, 9, 1),
            day(2026, 9, 15),
            day(2026, 10, 18),
        ),
        (
            "2026년 말까지 진행",
            day(2026, 6, 1),
            day(2026, 12, 31),
            day(2027, 1, 15),
        ),
        (
            "본 이벤트는 2026년 10월 13일에 종료됩니다",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        (
            "신청 마감일은 2026년 10월 13일입니다",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        (
            "본 이벤트는 2026년 10월 13일부로 종료",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        (
            "2026년 10월 13일을 끝으로 종료",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        (
            "2026.10.13 이벤트 종료",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        (
            "10. 13.(화) 오후 6시 마감",
            day(2026, 10, 1),
            day(2026, 10, 12),
            day(2026, 10, 14),
        ),
        // Two-digit year first (26/12/31) or last (DD/MM/YY 2031-12-26):
        // every valid reading counts.
        (
            "이벤트 기간 ~26/12/31까지",
            day(2026, 10, 1),
            day(2026, 12, 30),
            day(2027, 1, 1),
        ),
        (
            "Offer valid until 03/31/2027",
            day(2026, 10, 1),
            day(2027, 3, 1),
            day(2027, 4, 1),
        ),
        // Day or month first: the earlier reading (12 October) decides.
        (
            "Valid until 12/10/2026",
            day(2026, 10, 1),
            day(2026, 10, 11),
            day(2026, 10, 13),
        ),
    ];
    for (text, seen, on, passed) in cases {
        assert!(
            !has_past_end_at(text, &clock(on, 9, Some(seen))),
            "{text} on {on}"
        );
        assert!(
            has_past_end_at(text, &clock(passed, 9, Some(seen))),
            "{text} on {passed}"
        );
    }
    // The time of day on the end day.
    let text = "2026년 10월 13일 오후 6시까지";
    assert!(!has_past_end_at(
        text,
        &clock(day(2026, 10, 13), 17, Some(day(2026, 10, 1)))
    ));
    assert!(has_past_end_at(
        text,
        &clock(day(2026, 10, 13), 18, Some(day(2026, 10, 1)))
    ));
    assert!(has_past_end_at(
        "2026년 10월 18일 오전 10시까지",
        &clock(day(2026, 10, 18), 11, None)
    ));
    // A yearless or relative end with no first sight is read as passed.
    assert!(has_past_end_at(
        "~ 12월 31일까지",
        &clock(day(2026, 10, 18), 0, None)
    ));
    assert!(has_past_end_at(
        "이번 달 말까지",
        &clock(day(2026, 10, 18), 0, None)
    ));
    assert!(!has_past_end_at(
        "~ 12월 31일까지",
        &clock(day(2026, 10, 18), 0, Some(day(2026, 10, 18)))
    ));
    // A start with an offer verb is a start; a shipping date is not.
    assert!(has_future_start_at(
        "2026년 11월 1일부터 신청 가능",
        &Clock::at(day(2026, 10, 18))
    ));
    assert!(!has_future_start_at(
        "경품은 2026년 11월 10일 이후 순차 발송됩니다",
        &Clock::at(day(2026, 10, 18))
    ));
}

#[test]
fn a_condition_only_exempts_the_ending_it_governs() {
    for active in [
        "재고 소진 시 조기 종료될 수 있습니다",
        "※ 재고 소진 시 조기 종료",
        "종료 시 자동으로 해지됩니다",
        "마감 임박",
        "마감 D-5",
        "신청 마감까지 3일 남았어요",
        "자동결제 해지·중단 방법",
        "포인트 유효기간 만료 안내",
        "취소 및 환불 안내",
        "결제 후 주문 취소는 마이페이지에서 가능합니다",
        "이벤트 종료: 2099.12.31",
        "응모 D-3",
        "가입 후 D+30일 이내 첫 구매 시 적용",
    ] {
        assert_eq!(text_ending(active), None, "{active}");
    }
    for ended in [
        "당사 사정에 따라 본 이벤트를 조기 종료합니다.",
        "이벤트는 종료합니다(재고 소진 시 조기 종료될 수 있음)",
        "선착순 마감",
        "이벤트 종료 (문의 사항이 있을 경우 고객센터로 연락 바랍니다)",
        "판매 종료, 재입고 시 알림 신청 가능",
        "This offer has ended; if you have questions, contact us.",
        "Sold out - restock dates may vary",
        "선착순 모집이 완료되었습니다.",
        "혜택 제공이 끝난 이벤트입니다",
        "지금은 신청 기간이 아닙니다.",
        "응모가 마무리되었습니다. 참여해 주셔서 감사합니다.",
        "접수가 모두 끝나 신청하실 수 없습니다",
        "This promotion has finished.",
        "이벤트가 취소되었습니다",
        "응모 D+3",
        "모집 완료",
        "조기 마감합니다.다음 이벤트는 11월 중 오픈 예정입니다",
        "이벤트가 종료되어 지금은 응모하실 수 있는 기간이 아닙니다",
        // A trigger exempts only its own clause.
        "선착순 마감, 재고 소진 시 조기 종료",
        "이벤트 종료 · 재고 소진 시 조기 종료",
        "이벤트 종료; 재고 소진 시 조기 종료",
        "현재 응모 불가합니다",
    ] {
        assert!(text_ending(ended).is_some(), "{ended}");
    }
}

#[test]
fn navigation_and_unselected_tabs_are_skipped_and_every_other_control_is_read() {
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p>{extra}</section>"
        ))
    };
    for skipped in [
        "<nav><a href=\"/on\">진행중</a> <a href=\"/off\">종료된 이벤트</a></nav>",
        "<ul class=\"gnb\"><li><a href=\"/on\">진행중 이벤트</a></li><li><a href=\"/off\">지난 이벤트</a></li></ul>",
        "<div role=\"tablist\"><button role=\"tab\" aria-selected=\"true\">진행중</button><button role=\"tab\">종료</button></div>",
    ] {
        assert!(adopted_on(&page(skipped), QUOTE).confirms(), "{skipped}");
    }
    for read in [
        "<button type=\"button\" disabled>신청 마감</button>",
        "<a href=\"#none\" class=\"btn_end\">이벤트 종료</a>",
        "<div role=\"tablist\"><button role=\"tab\">진행중</button><button role=\"tab\" aria-selected=\"true\">종료</button></div>",
        "<h3><a href=\"/ended\">마감된 혜택</a></h3>",
        "<div role=\"button\">마감</div>",
        "<a href=\"#none\"><img src=\"/end.png\" alt=\"종료된 이벤트입니다\"></a>",
        "<form><input type=\"submit\" value=\"응모 마감\" disabled></form>",
        "<svg><text x=\"0\" y=\"15\">종료</text></svg>",
        "<ul class=\"cards\"><li><a href=\"/e/1\" class=\"card\"><span>마감</span><p>다른 혜택 카드</p></a></li></ul>",
    ] {
        assert!(!adopted_on(&page(read), QUOTE).confirms(), "{read}");
    }
}

#[test]
fn footers_asides_and_terms_notices_are_not_read_page_wide() {
    let with = |main_extra: &str, outside: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p></section>{main_extra}{outside}"
        ))
    };
    for outside in [
        "<aside><h3>지난 소식</h3><p>추석 이벤트 ~2026.09.30</p><p>여름 이벤트가 종료되었습니다.</p></aside>",
        "<div class=\"sidebar\"><p>여름 이벤트가 종료되었습니다.</p></div>",
        "<footer><p>2025년 이벤트는 종료되었습니다.</p></footer>",
        "<div class=\"top-notice\"><p>2099년 11월 1일부터 개정 약관이 적용됩니다.</p></div>",
    ] {
        let resolution = adopted_on(&with("", outside), QUOTE);
        assert!(resolution.confirms(), "{outside}: {resolution:?}");
    }
    for main in [
        "<div class=\"notice\"><p>여름 이벤트가 종료되었습니다.</p></div>",
        "<div class=\"notice\"><p>추석 이벤트 ~2026.09.30</p></div>",
        "<div class=\"top-notice\"><p>2099년 11월 1일부터 신청이 시작됩니다.</p></div>",
    ] {
        assert!(!adopted_on(&with(main, ""), QUOTE).confirms(), "{main}");
    }
}

#[test]
fn a_label_cell_is_read_with_its_value_cell() {
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p>{extra}</section>"
        ))
    };
    for active in [
        "<dl><dt>마감일</dt><dd>2099.12.31</dd></dl>",
        "<table><tr><th>이벤트 종료일</th><td>2099.12.31</td></tr></table>",
    ] {
        assert!(adopted_on(&page(active), QUOTE).confirms(), "{active}");
    }
    for ended in [
        "<dl><dt>마감일</dt><dd>2026.10.13</dd></dl>",
        "<table><tr><th>이벤트 종료일</th><td>2026.10.13</td></tr></table>",
        // A label without a date: read as ended.
        "<table><tr><th>판매 종료</th><td>상시 판매</td></tr></table>",
    ] {
        assert!(!adopted_on(&page(ended), QUOTE).confirms(), "{ended}");
    }
}

#[test]
fn only_standalone_furniture_is_folded_and_cells_and_states_are_kept() {
    let page = |note: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>{note}</p></section>"
        ))
    };
    for (one, two) in [
        (
            "댓글 3개 이상 작성 시 응모 완료",
            "댓글 10개 이상 작성 시 응모 완료",
        ),
        (
            "친구 추천 3명 달성 시 5,000원 적립",
            "친구 추천 5명 달성 시 5,000원 적립",
        ),
        (
            "좋아요 100개 달성 시 쿠폰 지급",
            "좋아요 1000개 달성 시 쿠폰 지급",
        ),
        (
            "가입 후 D+30일 이내 첫 구매 시 적용",
            "가입 후 D+7일 이내 첫 구매 시 적용",
        ),
        ("응모 D-3", "응모 D+3"),
        (
            "이용 2시간 전까지 예약 시 적용",
            "이용 24시간 전까지 예약 시 적용",
        ),
        (
            "회원 등록일 2026.01.01 이후 가입 고객 대상",
            "회원 등록일 2026.06.01 이후 가입 고객 대상",
        ),
        ("쿠폰 1,2차 지급", "쿠폰 12차 지급"),
        ("2. 5% 적립", "2.5% 적립"),
    ] {
        assert_ne!(
            digest_of(&page(one)),
            digest_of(&page(two)),
            "{one} / {two}"
        );
    }
    // A counter next to its board label in its own cell is folded.
    let board = |n: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><dl><dt>조회</dt><dd>{n}</dd></dl></section>"
        ))
    };
    assert_eq!(digest_of(&board("123")), digest_of(&board("4,567")));
    // A cell's span, a block moved to another cell, a control's state.
    let table = |rows: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><table>{rows}</table></section>"
        ))
    };
    assert_ne!(
        digest_of(&table(
            "<tr><th>대학생</th><td rowspan=\"2\">10% 할인</td></tr><tr><th>대학원생</th></tr>"
        )),
        digest_of(&table(
            "<tr><th>대학생</th><td>10% 할인</td></tr><tr><th>대학원생</th></tr>"
        ))
    );
    assert_ne!(
        digest_of(&table(
            "<tr><td><p>대학생</p><p>대학원생</p></td><td><p>교직원</p></td></tr>"
        )),
        digest_of(&table(
            "<tr><td><p>대학생</p></td><td><p>대학원생</p><p>교직원</p></td></tr>"
        ))
    );
    let button = |attrs: &str| page(&format!("</p><button {attrs}>신청하기</button><p>"));
    assert_ne!(digest_of(&button("")), digest_of(&button("disabled")));
}

#[test]
fn structured_data_states_are_read() {
    let page = |head: &str| {
        shell(&format!(
            "{head}<section><h2>학생 할인</h2><p>{QUOTE}</p></section>"
        ))
    };
    let ld =
        |json: &str| format!("<script type=\"application/ld+json; charset=utf-8\">{json}</script>");
    for active in [
        ld(r#"{"@type":"Offer","availability":"https://schema.org/InStock","validThrough":"2099-12-31"}"#),
        ld(r#"{"@type":"Event","eventStatus":"https://schema.org/EventScheduled","endDate":"2099-12-31"}"#),
        "<div itemscope itemtype=\"https://schema.org/Offer\"><meta itemprop=\"validThrough\" content=\"2099-12-31\"></div>".to_string(),
    ] {
        assert!(adopted_on(&page(&active), QUOTE).confirms(), "{active}");
    }
    // The page's own item (single, or named as the page) that ended: read,
    // and hidden in live, but trusted only as `mainEntity` or at this URL.
    for (head, reason) in [
        (ld(r#"{"@type":"Offer","availability":"https://schema.org/SoldOut"}"#), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@type":"Offer","availability":"https://schema.org/Discontinued"}"#), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@type":"Event","eventStatus":"https://schema.org/EventCancelled"}"#), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@context":{"schema":"https://schema.org/"},"schema:validThrough":"2026-10-13"}"#), PAGE_STRUCTURED_OTHER),
        ("<div itemscope><meta itemprop=\"validThrough\" content=\"2026-10-13\"></div>".to_string(), PAGE_STRUCTURED_OTHER),
        ("<div itemscope><link itemprop=\"availability\" href=\"https://schema.org/SoldOut\"></div>".to_string(), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@type":"Offer","availability":"https://schema.org/OutOfStock"}"#), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@type":"Offer","validFrom":"2099-01-01"}"#), PAGE_STRUCTURED_OTHER),
        (ld(r#"{"@type":"WebPage","mainEntity":{"@type":"Offer","availability":"https://schema.org/SoldOut"}}"#), PAGE_STRUCTURED_OTHER),
        (ld(r##"{"@graph":[{"@type":"WebPage","mainEntity":{"@id":"#e"}},{"@id":"#e","@type":"Event","endDate":"2026-10-13"}]}"##), PAGE_STRUCTURED_OTHER),
        ("<div itemscope itemtype=\"https://schema.org/WebPage\"><div itemprop=\"mainEntity\" itemscope itemtype=\"https://schema.org/Event\"><meta itemprop=\"eventStatus\" content=\"https://schema.org/EventCancelled\"></div></div>".to_string(), PAGE_STRUCTURED_OTHER),
    ] {
        assert_eq!(adopted_on(&page(&head), QUOTE), Resolution::Ambiguous(reason.into()), "{head}");
    }
    // Trusted, and shown on the page (the end day written outside the
    // offer's section): `structured_ended`.
    let shown = shell(&format!(
        "{}<section><h2>학생 할인</h2><p>{QUOTE}</p></section><section><h2>안내</h2><p>행사 일정 2026.10.13</p></section>",
        ld(r##"{"@graph":[{"@type":"WebPage","mainEntity":{"@id":"#e"}},{"@id":"#e","@type":"Event","endDate":"2026-10-13"}]}"##)
    ));
    assert_eq!(
        adopted_on(&shown, QUOTE),
        Resolution::Ambiguous(PAGE_STRUCTURED_ENDED.into())
    );
    // Never a publication, founding, review or price-validity date, and
    // never a key the trusted reader does not read.
    for head in [
        ld(r#"{"@type":"WebPage","mainEntity":{"@type":"Article","datePublished":"2025-03-02"}}"#),
        ld(
            r#"{"@type":"WebPage","mainEntity":{"@type":"Product","offers":{"@type":"Offer","priceValidUntil":"2025-12-31"}}}"#,
        ),
        ld(r#"{"@type":"Organization","foundingDate":"1956-03-01"}"#),
    ] {
        assert!(adopted_on(&page(&head), QUOTE).confirms(), "{head}");
    }
    // The one offer or event at this page's URL is trusted; at another URL,
    // or one of two at this URL, it is not.
    let at = |json: &str, url: &str| {
        // The end day is written on the page too (corroboration).
        let html = shell(&format!(
            "{}<section><h2>학생 할인</h2><p>{QUOTE}</p></section><section><h2>안내</h2><p>행사 일정 2026.10.13</p></section>",
            ld(json)
        ));
        resolve(
            &PageView::new(&html, &visible_text(&html)).with_url(url),
            &StoredFact {
                quote: QUOTE,
                baseline: None,
                ages: no_ages(),
                adoption_open: true,
                anchors: &Anchors::default(),
            },
        )
    };
    let offer =
        r#"{"@type":"Offer","url":"https://b.example/student/","validThrough":"2026-10-13"}"#;
    let ended = Resolution::Ambiguous(PAGE_STRUCTURED_ENDED.into());
    assert_eq!(at(offer, "https://B.example/student#top"), ended);
    assert_ne!(at(offer, "https://b.example/other"), ended);
    let two = format!("[{offer},{offer}]");
    assert_ne!(at(&two, "https://b.example/student"), ended);
}

#[test]
fn the_offers_own_section_gives_the_most_certain_reason() {
    let page = |section_note: &str, elsewhere: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>{section_note}</p></section><section><h2>다른 소식</h2><p>{elsewhere}</p></section>"
        ))
    };
    let reason = |html: &str| match adopted_on(html, QUOTE) {
        Resolution::Ambiguous(reason) => reason_key(&reason),
        _ => None,
    };
    assert_eq!(
        reason(&page("이벤트가 종료되었습니다.", "")),
        Some("section_ended_past")
    );
    assert_eq!(
        reason(&page("이벤트 기간 ~ 2026.10.13", "")),
        Some("section_date_past")
    );
    assert_eq!(reason(&page("판매 종료", "")), Some("section_ended"));
    assert_eq!(reason(&page("오픈 예정", "")), Some("section_not_started"));
    assert_eq!(
        reason(&page("학생 인증은 온라인 구매 시 제외됩니다.", "")),
        Some("section_excluded")
    );
    assert_eq!(
        reason(&page("", "여름 이벤트가 종료되었습니다.")),
        Some("ending_scan")
    );
    assert_eq!(
        reason(&page("", "여름 이벤트 ~ 2026.09.30")),
        Some("date_past")
    );
    // Another offer's coming opening elsewhere is not this offer's state.
    assert!(adopted_on(&page("", "다음 이벤트는 11월 중 오픈 예정입니다"), QUOTE).confirms());
}

#[test]
fn a_digest_of_another_version_does_not_decide() {
    let mut fact = confirmed(QUOTE, no_ages());
    fact.anchors.fingerprint.as_mut().unwrap().page = "v1:0123".into();
    assert!(resolve_on(&fact, &original()).confirms());
}

#[test]
fn a_passed_date_with_a_weekday_in_the_section_is_the_sections_reason() {
    let html = shell(&format!(
        "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>신청 기간: 2026. 10. 13.(화)까지</p></section>"
    ));
    let mut page = view(&html);
    page.today = day(2026, 10, 18);
    let resolution = resolve(
        &page,
        &StoredFact {
            quote: QUOTE,
            baseline: None,
            ages: no_ages(),
            adoption_open: true,
            anchors: &Anchors::default(),
        },
    );
    assert_eq!(resolution, Resolution::Ambiguous(SECTION_DATE_PAST.into()));
}

#[test]
fn exclusions_hide_only_when_they_name_the_offer_and_only_at_first_sight() {
    let page = |own: &str, note: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}{own}</p><ul class=\"terms\"><li>{note}</li></ul></section>"
        ))
    };
    // Ordinary commerce exclusions, in a note or the statement's element.
    for (own, note) in [
        ("", "※ 다른 쿠폰과 중복 사용은 불가합니다."),
        ("", "※ 일부 품목은 할인 대상에서 제외됩니다."),
        ("", "※ 타 쿠폰과 중복 적용 불가"),
        ("", "※ 온라인 주문에 한해 무료 배송"),
        (" (중복 사용 불가)", ""),
        ("", "※ 배송비는 할인에서 제외됩니다."),
    ] {
        let resolution = adopted_on(&page(own, note), QUOTE);
        assert!(resolution.confirms(), "{own}{note}: {resolution:?}");
    }
    // An exclusion naming the offer's audience or value, cancelling the
    // offer itself, or in the statement's own element.
    for (own, note) in [
        ("", "※ 휴학생 및 졸업생은 학생 할인 대상에서 제외됩니다."),
        ("", "※ 학생 할인 적용 불가 매장이 있습니다."),
        ("", "※ 본 혜택은 온라인 구매 시 제외됩니다."),
        ("", "※ 10% 할인은 일부 회원에게 적용되지 않습니다."),
        (" (온라인 구매 제외)", ""),
        (" 단, 휴학생 X", ""),
    ] {
        let resolution = adopted_on(&page(own, note), QUOTE);
        assert_eq!(
            resolution,
            Resolution::Ambiguous(SECTION_EXCLUDED.into()),
            "{own}{note}"
        );
    }
    // On a page that reads as at its first sight, an exclusion already
    // there then never hides it.
    let html = page("", "※ 휴학생 및 졸업생은 학생 할인 대상에서 제외됩니다.");
    let fact = Confirmed {
        quote: QUOTE.to_string(),
        // The window adopted at its first confirmation, as stored.
        baseline: PageEvidence::from_html(&html)
            .window_signature(QUOTE)
            .unwrap(),
        anchors: Anchors {
            fingerprint: capture(&view(&html), QUOTE),
            ..Anchors::default()
        },
        ages: no_ages(),
    };
    assert!(resolve_on(&fact, &html).confirms());
}

#[test]
fn the_time_of_day_on_the_end_day_is_read_at_the_injected_moment() {
    let html = shell(&format!(
        "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>신청 기간: 2026년 10월 18일 오전 10시까지</p></section>"
    ));
    let fact = StoredFact {
        quote: QUOTE,
        baseline: None,
        ages: no_ages(),
        adoption_open: true,
        anchors: &Anchors::default(),
    };
    let at = |hour: u32, minute: u32| {
        let now = day(2026, 10, 18).and_hms_opt(hour, minute, 0).unwrap();
        resolve(&PageView::at(&html, &visible_text(&html), now), &fact)
    };
    assert!(at(9, 59).confirms(), "before the stated time");
    assert_eq!(at(10, 0), Resolution::Ambiguous(SECTION_DATE_PAST.into()));
    // Without an injected moment the page is read at the end of its day,
    // whatever the wall clock says.
    let mut page = view(&html);
    page.today = day(2026, 10, 18);
    page.now = day(2026, 10, 18).and_hms_opt(23, 59, 59).unwrap();
    assert_eq!(
        resolve(&page, &fact),
        Resolution::Ambiguous(SECTION_DATE_PAST.into())
    );
    let mut later_today = view(&html);
    later_today.today = day(2026, 10, 18);
    assert!(!resolve(&later_today, &fact).confirms());
}

// ── round 9: precision of the hide_only default reasons ───────────────────

/// The r8 review's active probes (P1–P16): under the default hide_only
/// allowlist none is hidden (each may still hide in live with another,
/// non-allowlisted reason).
#[test]
fn active_probes_are_never_hidden_by_the_default_allowlist() {
    const Q: &str = "학생 인증 시 10% 할인";
    let page = |main: &str| {
        format!(
            "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>학생 혜택 | 브랜드</title></head>\
             <body><header class=\"hd\"><h1 class=\"logo\"><a href=\"/\">브랜드</a></h1>\
             <nav class=\"gnb\"><ul><li><a href=\"/event\">진행 이벤트</a></li><li><a href=\"/event/past\">지난 이벤트</a></li></ul></nav></header>\
             <main id=\"content\">{main}</main>\
             <footer class=\"ft\"><a href=\"/terms\">이용약관</a> <a href=\"/privacy\">개인정보처리방침</a><p>고객센터 1588-1234</p></footer></body></html>"
        )
    };
    let offer = |extra: &str| {
        page(&format!(
            "<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p>{extra}</section>"
        ))
    };
    let past_list = "<div class=\"event-list\"><h3>다른 이벤트</h3><ul>\
        <li><a href=\"/e/1\"><strong>여름방학 학생 특가</strong><span class=\"date\">2026.07.01 ~ 2026.08.31</span></a></li>\
        <li><a href=\"/e/2\"><strong>신학기 노트북 할인</strong><span class=\"date\">2026.02.01 ~ 2026.03.31</span></a></li></ul></div>";
    let ld_product = "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"name\":\"추천 상품 A\",\"offers\":{\"@type\":\"Offer\",\"price\":\"39000\",\"priceCurrency\":\"KRW\",\"priceValidUntil\":\"2025-12-31\"}}</script>";
    let ld_events = "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"ItemList\",\"itemListElement\":[{\"@type\":\"Event\",\"name\":\"지난 팝업 스토어\",\"startDate\":\"2026-08-01\",\"endDate\":\"2026-08-15\"}]}</script>";
    let micro_soldout = "<div class=\"related\" itemscope itemtype=\"https://schema.org/Product\"><span itemprop=\"name\">함께 보면 좋은 상품</span>\
        <div itemprop=\"offers\" itemscope itemtype=\"https://schema.org/Offer\"><link itemprop=\"availability\" href=\"https://schema.org/SoldOut\"><span>일시품절</span></div></div>";
    let cases: Vec<(&str, String)> = vec![
        ("P1", page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p><p>기간: 2026.10.01 ~ 2026.12.31</p></section>{past_list}"))),
        ("P1b", offer(&format!("<p>기간: 2026.10.01 ~ 2026.12.31</p>{past_list}"))),
        ("P2", offer("<p>1차 신청 기간: 2026.09.01 ~ 2026.09.30</p><p>2차 신청 기간: 2026.10.01 ~ 2026.11.30</p>")),
        ("P3", offer("<p>1차 모집 마감 / 2차 모집 진행 중 (~2026.11.30)</p>")),
        ("P4", offer("<p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p><p>1차 당첨자 발표: 2026.10.10(토)</p>")),
        ("P4b", offer("<p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p><p>1차 당첨자 경품 발송: 10월 15일까지</p>")),
        ("P5", offer("<p class=\"meta\">등록일 2026.09.01 | 수정일 2026.09.15</p>")),
        ("P6", offer("<p>학생 인증이 완료되었다면 쿠폰함을 확인하세요.</p>")),
        ("P7", offer("<p>수능 끝난 수험생을 위한 특별 혜택도 함께 확인하세요.</p>")),
        ("P8", offer("<p>인증을 마쳤다면 바로 사용할 수 있어요.</p>")),
        ("P9", offer("<p>기말고사 끝나고 떠나는 여행도 학생 요금으로!</p>")),
        ("P10", offer("<p>※ 2025년 12월 31일까지 가입한 회원은 기존 혜택이 유지됩니다.</p>")),
        ("P11", offer("<p>※ 2026년 2월까지 졸업(예정)한 학생도 대상입니다.</p>")),
        ("P12", page(&format!("{ld_product}<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>"))),
        ("P13", page(&format!("{ld_events}<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>"))),
        ("P14", page(&format!("<section class=\"evt\"><h2>학생 할인</h2><p>{Q}</p></section>{micro_soldout}"))),
        ("P15", offer("<p>쿠폰 유효기간: 발급일로부터 30일</p>")),
        ("P16", offer("<p>※ 전년도 이벤트(2025.10.01~2025.12.31) 당첨자는 참여할 수 없습니다.</p>")),
    ];
    let now = day(2026, 10, 18).and_hms_opt(12, 0, 0).unwrap();
    let mut live_hidden = Vec::new();
    for (id, html) in &cases {
        let resolution = resolve(
            &PageView::at(html, &visible_text(html), now),
            &StoredFact {
                quote: Q,
                baseline: None,
                ages: no_ages(),
                adoption_open: true,
                anchors: &Anchors::default(),
            },
        );
        let key = match &resolution {
            Resolution::Ambiguous(reason) => reason_key(reason),
            _ => None,
        };
        assert!(
            !key.is_some_and(|k| super::super::DEFAULT_HIDE_REASONS.contains(&k)),
            "{id} hidden in hide_only: {resolution:?}"
        );
        if !resolution.confirms() {
            live_hidden.push((*id, key));
        }
    }
    eprintln!("active probes still hidden in live: {live_hidden:?}");
}

#[test]
fn the_trusted_reasons_still_read_this_offers_own_ending_and_period() {
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p>{extra}</section>"
        ))
    };
    let reason = |html: &str| match adopted_on(html, QUOTE) {
        Resolution::Ambiguous(reason) => reason_key(&reason),
        _ => None,
    };
    for (extra, key) in [
        // Two signals: the trusted reason.
        (
            "<dl><dt>이벤트 기간</dt><dd>2026.09.01 ~ 2026.10.13</dd></dl><p>본 이벤트는 종료되었습니다.</p>",
            "section_ended_confirmed",
        ),
        (
            "<table><tr><th>행사 기간</th><td>2026년 9월 1일 ~ 10월 13일</td></tr></table><p>종료된 이벤트입니다.</p>",
            "section_ended_confirmed",
        ),
        (
            "<p>기간: 2026.09.01 ~ 2026.10.13</p><p>해당 프로모션은 조기 종료되었습니다.</p>",
            "section_ended_confirmed",
        ),
        // One signal: a shadow metric.
        ("<p>이벤트가 종료되었습니다.</p>", "section_ended_past"),
        (
            "<p>본 혜택은 2026년 10월 15일 마감되었습니다.</p>",
            "section_ended_past",
        ),
        ("<p>종료된 이벤트입니다.</p>", "section_ended_past"),
        (
            "<p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>",
            "section_date_past",
        ),
        ("<p>신청 마감일 2026년 10월 13일</p>", "section_date_past"),
        ("<p>2026.09.01 ~ 2026.10.13</p>", "section_date_past"),
        // Read, but not as this offer's own ending or period.
        ("<p>조기 마감되었습니다.</p>", "section_ended"),
        (
            "<p>접수가 모두 끝나 신청하실 수 없습니다</p>",
            "section_ended",
        ),
        ("<p>9월 말까지 진행</p>", "section_date_other"),
    ] {
        assert_eq!(reason(&page(extra)), Some(key), "{extra}");
    }
    // A phase's period (`1차`) is not read as a deadline of the offer (the
    // r8 probe P2: the next phase runs).
    assert!(adopted_on(
        &page("<p>1차 신청 기간: 2026.09.01 ~ 2026.09.30</p>"),
        QUOTE
    )
    .confirms());
    // A JSON-LD item naming this page is read; one naming another item is
    // not.
    let ld = |name: &str, until: &str| {
        shell(&format!(
            "<script type=\"application/ld+json\">{{\"@type\":\"Offer\",\"name\":\"{name}\",\"validThrough\":\"{until}\"}}</script><section><h2>학생 할인</h2><p>{QUOTE}</p></section>"
        ))
    };
    assert_eq!(
        reason(&ld("t", "2026-10-13")),
        Some("structured_other"),
        "names the page title: read, not trusted"
    );
    // The structured end matches the labelled period: two signals; on
    // another day it is no second signal.
    let matched = |end: &str| {
        shell(&format!(
            "<script type=\"application/ld+json\">{{\"@type\":\"WebPage\",\"mainEntity\":{{\"@type\":\"Event\",\"endDate\":\"{end}\"}}}}</script><section><h2>학생 할인</h2><p>{QUOTE}</p><p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p></section>"
        ))
    };
    assert_eq!(
        reason(&matched("2026-10-13")),
        Some("section_ended_confirmed")
    );
    assert_eq!(reason(&matched("2026-10-12")), Some("section_date_past"));
    assert!(adopted_on(&ld("추천 상품 A", "2026-10-13"), QUOTE).confirms());
    let main = shell(&format!(
        "<script type=\"application/ld+json\">{{\"@type\":\"WebPage\",\"mainEntity\":{{\"@type\":\"Event\",\"name\":\"다른 이름\",\"eventStatus\":\"https://schema.org/EventCancelled\"}}}}</script><section><h2>학생 할인</h2><p>{QUOTE}</p></section>"
    ));
    assert_eq!(
        reason(&main),
        Some("structured_other"),
        "mainEntity is the page's own, but nothing visible says so"
    );
}

/// A quote that is only in hidden text never confirms (lead decision §23):
/// a collapsed panel with its toggle, a hiding class, a popup, an inactive
/// tab panel, `visibility:hidden`, a template.
#[test]
fn a_quote_in_hidden_text_never_confirms() {
    for hidden in [
        format!("<div><button aria-expanded=\"false\">혜택 상세</button></div><div class=\"none\" aria-hidden=\"true\"><p>{QUOTE}</p></div>"),
        format!("<button aria-controls=\"p1\">혜택 상세</button><div id=\"p1\" hidden><p>{QUOTE}</p></div>"),
        format!("<div class=\"c-accordion-header\"><span>Q</span></div><div class=\"collapse\" style=\"display:none\"><p>{QUOTE}</p></div>"),
        format!("<button aria-expanded=\"false\">혜택</button><p class=\"sr-only\">{QUOTE}</p>"),
        format!("<button aria-expanded=\"false\">혜택</button><div class=\"layer-popup\" style=\"display:none\"><p>{QUOTE}</p></div>"),
        format!("<div class=\"collapse\" style=\"visibility:hidden\"><p>{QUOTE}</p></div>"),
        format!("<button aria-expanded=\"false\">탭</button><div role=\"tabpanel\" hidden><p>{QUOTE}</p></div>"),
        format!("<template><p>{QUOTE}</p></template>"),
        format!("<div style=\"display:none\"><p>{QUOTE}</p></div>"),
    ] {
        let html = shell(&format!("<section><h2>학생</h2>{hidden}</section>"));
        assert!(!adopted_on(&html, QUOTE).confirms(), "{hidden}");
    }
}

/// Tiles and links are read like any other block: a sold-out badge on a
/// product tile, an end banner in a link, a control link.
#[test]
fn linked_tiles_and_links_are_read_page_wide() {
    for read in [
        "<ul class=\"products\"><li><a href=\"/p/1\"><div>노트북</div><div>일시품절</div></a></li><li><a href=\"/p/2\"><div>태블릿</div><div>구매하기</div></a></li></ul>".to_string(),
        "<a href=\"/event/list\"><div>본 이벤트는 종료되었습니다</div></a>".to_string(),
        "<div><a href=\"#none\"><div>종료된 이벤트입니다</div></a></div>".to_string(),
    ] {
        let html = shell(&format!("<section><h2>학생 할인</h2><p>{QUOTE}</p></section>{read}"));
        assert!(!adopted_on(&html, QUOTE).confirms(), "{read}");
    }
}

/// Both extractors read an age range the same way (`19~24세`: the value
/// signature holds `24세`, the hand reader adds the range start).
#[test]
fn both_extractors_read_an_age_range() {
    for text in [
        "대학생(19~24세)은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.",
        "만 19세 ~ 24세 대학생 10% 할인",
        "만 34세 이하 청년 10% 할인",
    ] {
        assert!(extractors_agree(text), "{text}");
    }
    assert_eq!(age_range_starts_b("19~24세"), vec![19]);
    assert_eq!(age_range_starts_b("2026-10-13 24세"), Vec::<i32>::new());
    assert!(age_range_starts_b("10~20% 할인").is_empty());
}

/// Property (lead decision §23): no ending clause of the r1/r2 probes turns
/// from hidden to confirmed when a rule-context phrase is put before it or
/// when it is wrapped in a list item with a link, a link, or a collapsed
/// panel. The reader may only get stricter than `ea4a3f17`.
#[test]
fn rule_context_and_wrapping_never_turn_an_ending_into_a_confirmation() {
    let clauses = [
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
        "R1 대학생 멤버십 서비스는 9월 30일 종료되었으며 기존 회원의 경우 적립 포인트는 유지됩니다",
        "R2 학생 요금제는 2026년 9월 30일 기준으로 서비스가 종료되었습니다",
        "R3 시스템 개편 등으로 학생 할인 서비스가 종료되었습니다",
        "R4 학생 할인 서비스는 종료되었으나 기존 쿠폰은 사용 가능합니다",
        "R5 2026년 10월 1일부터 대학생 멤버십 서비스가 종료됩니다",
        "R6 학생 멤버십 종료 안내 기존 회원의 경우 포인트는 유지됩니다",
        "R7 대학생 요금제 가입 시 제공되던 학생 할인 서비스가 종료되었습니다",
        "R8 학생 회원 혜택은 10월부터 종료되었습니다",
        "R9 10월 1일부터 학생 할인 이용 종료",
        "U1 선착순 접수가 완료되었습니다.",
        "U2 접수완료",
        "U3 신청 접수가 모두 완료되었습니다",
        "U4 선착순 100명 참여가 완료되었습니다",
        "U5 응모 접수 완료",
        "F1 학생 인증을 하신 경우에도 현재 쿠폰 발급이 불가합니다",
        "F2 이미 인증하신 경우라도 신규 가입이 불가합니다",
        "F3 대상자인 경우에도 더 이상 신청이 불가능합니다",
        "E1 The education bundle is no longer available",
        "E2 Campus perks are unavailable",
        "E3 Free Pro for academics is no longer available",
        "S1 마  감",
    ];
    let prefixes = [
        "2026년 1월 1일 기준으로",
        "가입 시",
        "기존 회원의 경우",
        "10월 1일부터",
        "시스템 개편 등으로",
        "서비스가 개편되었으나",
    ];
    let section = format!("<section><h2>학생 할인</h2><p>{QUOTE}</p></section>");
    let mut checked = 0;
    for clause in clauses {
        if text_ending(&squash(clause)).is_none() {
            continue;
        }
        let mut variants: Vec<String> = prefixes
            .iter()
            .map(|p| format!("<p>{p} {clause}</p>"))
            .collect();
        variants.push(format!(
            "<ul><li>{clause} <a href=\"/notice/1\">자세히</a></li><li><a href=\"/notice/2\">다른 공지</a></li></ul>"
        ));
        variants.push(format!("<a href=\"/events\"><div>{clause}</div></a>"));
        variants.push(format!(
            "<button aria-expanded=\"false\">더보기</button><div hidden><p>{clause}</p></div>"
        ));
        for variant in variants {
            let html = shell(&format!(
                "{section}<section><h2>공지</h2>{variant}</section>"
            ));
            assert!(!adopted_on(&html, QUOTE).confirms(), "{variant}");
            checked += 1;
        }
    }
    assert!(checked >= 9 * 55, "{checked}");
}

/// The clause fuzz with the words the reviewer added (`안타깝게도`,
/// `아쉽게도`, `부득이하게`, `조기`): an offer or its event said to end
/// reads as an ending whatever adverb comes first, and the page never
/// confirms (expected outcomes; §31 retired the frozen-reader comparison).
#[test]
fn the_extra_fuzz_words_never_turn_an_ending_into_a_confirmation() {
    let subjects = [
        "이벤트가",
        "학생 할인이",
        "본 혜택은",
        "프로모션이",
        "쿠폰 발급이",
        "모집이",
        "판매가",
    ];
    let verbs = [
        "종료되었습니다",
        "마감되었습니다",
        "종료",
        "마감",
        "중단되었습니다",
        "소진되었습니다",
        "종료됩니다",
        "끝났습니다",
    ];
    let prefixes = [
        "안타깝게도 ",
        "아쉽게도 ",
        "부득이하게 ",
        "조기 ",
        "부득이하게 조기 ",
        "안타깝게도 조기 ",
    ];
    let section = format!("<section><h2>학생 할인</h2><p>{QUOTE}</p></section>");
    let mut checked = 0;
    for prefix in prefixes {
        for subject in subjects {
            for verb in verbs {
                for (a, b) in [("", ""), ("", " 조기"), (" 조기", "")] {
                    let clause = format!("{prefix}{subject}{a} {verb}{b}");
                    assert!(text_ending(&squash(&clause)).is_some(), "{clause}");
                    let html = shell(&format!(
                        "{section}<section><h2>공지</h2><p>{clause}</p></section>"
                    ));
                    assert!(!adopted_on(&html, QUOTE).confirms(), "{clause}");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 500, "{checked}");
}

// ── the current-fact rule: the offer's own section ───────────────────────

/// A sentence added to the offer's own section (a banner the ending scan
/// does not read) holds the fact for fresh extraction; text added in
/// another section does not. A fact captured before sections were read is
/// decided by its quote, value and place, and its section is captured at
/// its next confirmation.
#[test]
fn a_changed_offer_section_holds_the_fact_and_other_sections_do_not() {
    let fact = confirmed(QUOTE, no_ages());
    let banner = original().replace(
        "할인 쿠폰이 재발급됩니다.</p></div>",
        "할인 쿠폰이 재발급됩니다.</p></div><p class=\"banner\">곧 찾아올 혜택입니다</p>",
    );
    assert_eq!(
        resolve_on(&fact, &banner),
        Resolution::Ambiguous(SECTION_CHANGED.into())
    );
    let elsewhere = original().replace(
        "<section class=\"other\"><h2>멤버십</h2>",
        "<section class=\"other\"><h2>멤버십</h2><p>곧 찾아올 혜택입니다</p>",
    );
    let resolution = resolve_on(&fact, &elsewhere);
    assert!(resolution.confirms(), "{resolution:?}");

    let mut legacy = confirmed(QUOTE, no_ages());
    for identity in &mut legacy.anchors.fingerprint.as_mut().unwrap().identities {
        identity.section.clear();
    }
    // No stored section: decided by quote, value and place alone.
    let resolution = resolve_on(&legacy, &elsewhere);
    assert!(resolution.confirms(), "legacy: {resolution:?}");
    let recaptured = capture(&view(&elsewhere), QUOTE).unwrap();
    assert!(recaptured.identities.iter().all(|i| !i.section.is_empty()));
}

// ── the offer relation ────────────────────────────────────────────────────

#[test]
fn about_tells_this_offer_an_unstated_subject_and_another_subject_apart() {
    // The offer as its heading chain (and programme title) names it.
    let offer = OfferIdentity::new(["학생 할인"]);
    for clause in [
        "학생 할인은 9월 말까지 진행됩니다",
        "본 이벤트는 종료되었습니다",
        "이벤트 기간: 2026.09.01 ~ 2026.10.13",
        "This offer has ended",
        "추가 혜택은 10월 31일까지",
    ] {
        assert_eq!(about(clause, &offer), About::ThisOffer, "{clause}");
    }
    for clause in [
        "조기 마감되었습니다",
        "오늘까지만 진행",
        "SOLD OUT",
        "이번 달 말까지 진행",
    ] {
        assert_eq!(about(clause, &offer), About::Unstated, "{clause}");
    }
    for clause in [
        "인터넷 익스플로러(IE)는 6월 15일 지원이 종료되어 엣지에 최적화되어 있습니다",
        "학교 이메일 인증 가입자 : 만 28세가 되는 해 12월 31일까지 자격 유지",
        "Custom domains are not available",
        "포인트 유효기간은 적립일로부터 1년입니다",
    ] {
        assert_eq!(about(clause, &offer), About::Other, "{clause}");
    }
    // A word of its own name names it; a shared generic word does not.
    let galaxy = OfferIdentity::new(["갤럭시 캠퍼스 교육 할인"]);
    assert_eq!(
        about("갤럭시 캠퍼스 멤버십은 12월 31일까지", &galaxy),
        About::ThisOffer
    );
    assert_eq!(
        about("학교 이메일 인증 가입자 : 12월 31일까지 자격 유지", &galaxy),
        About::Other
    );
}

/// A passed deadline in the offer's section hides it only when the
/// deadline is about the offer (or states no subject): another subject's
/// deadline (a browser notice, a membership term) says nothing about it.
#[test]
fn a_deadline_hides_the_offer_only_when_it_is_about_the_offer() {
    let page = |note: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>{note}</p></section>"
        ))
    };
    for other in [
        "인터넷 익스플로러(IE)는 2026년 6월 15일까지만 지원합니다.",
        "학교 이메일 인증 가입자 : 2025년 12월 31일까지 자격 유지",
    ] {
        let resolution = adopted_on(&page(other), QUOTE);
        assert!(resolution.confirms(), "{other}: {resolution:?}");
    }
    for own in [
        "학생 할인은 2026년 9월 30일까지 진행됩니다.",
        "2026년 9월 30일까지 진행",
    ] {
        let resolution = adopted_on(&page(own), QUOTE);
        assert!(!resolution.confirms(), "{own}: {resolution:?}");
    }
}

/// Off its own section, an ending counts only when it names this offer by a
/// pointer word or a word of its name; an offer noun alone is any offer's.
#[test]
fn off_section_text_must_name_this_offer() {
    let offer = OfferIdentity::new(["학생 할인"]);
    assert!(names_this_offer(
        "학생 할인 이벤트가 종료되었습니다",
        &offer
    ));
    assert!(names_this_offer("본 이벤트는 종료되었습니다", &offer));
    assert!(!names_this_offer(
        "서류 인증 신청이 완료 되었습니다",
        &offer
    ));
    assert_eq!(
        about("서류 인증 신청이 완료 되었습니다", &offer),
        About::ThisOffer
    );
}

#[test]
fn a_verb_modifier_is_no_subject_and_a_marked_footnote_is_the_offers_own() {
    let offer = OfferIdentity::new(["학생할인"]);
    assert_eq!(about("오늘까지만 진행되는 혜택", &offer), About::Unstated);
    assert_eq!(about("할인을 받을 수 있는 혜택", &offer), About::Unstated);
    // A footnote list after the next section, pointed to by `※1`.
    let html = shell(
        "<section><h2>학생 할인</h2><p>학생 인증 시 10% 할인※1</p></section>\
         <section><h2>교직원 할인</h2><p>교직원 인증 시 5% 할인※2</p></section>\
         <div class=\"footnotes\"><p>※1 온라인 결제 시 적용</p><p>※2 매장 결제 시 적용</p></div>",
    );
    let page = view(&html);
    let i = holders(&page, &fold_str("학생 인증 시 10% 할인"))
        .into_iter()
        .next()
        .unwrap();
    let notes: Vec<String> = footnotes(&page.texts, offer_section(&page, i))
        .into_iter()
        .map(|f| page.texts.of(f))
        .collect();
    assert_eq!(notes, ["※1 온라인 결제 시 적용"]);
}

/// The way back (re-baselining a held fact on its new section) must never
/// re-baseline a section that gained an ending, a pause, a refusal or an
/// exclusion: every phrase of the generated corpus's own wording.
///
/// TODO(way-back): fails today (release review 2026-10-28: `곧 찾아올
/// 혜택입니다`, `지금은 신청하실 수 없어요`, `신입생은 대상에서 빠집니다`,
/// `Paused until further notice`, … re-baseline). The proper fix: read the
/// new sentences with the full first-sight readers (endings, not-started
/// forms, `exclusion_reason`/`says_excluded`), re-baseline only when every
/// new sentence reads as benign, and have the second reader read the new
/// page rather than the stored quote. Until then the way back stays off
/// (`CORPORATE_EVIDENCE_WAY_BACK_ENABLED`).
#[test]
#[ignore = "TODO(way-back): the way back reads new text with the ending reader only"]
fn the_way_back_never_rebaselines_a_gone_phrase() {
    use crate::corporate_auto::evidence::{fresh_anchors, FreshExtraction};
    let page = |extra: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p><p>30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p>{extra}</section>"
        ))
    };
    let before = page("");
    let fact = confirm_on(&before, QUOTE, no_ages()).unwrap();
    let stored = StoredFact {
        quote: QUOTE,
        baseline: Some(&fact.baseline),
        ages: no_ages(),
        adoption_open: false,
        anchors: &fact.anchors,
    };
    let now = chrono::Utc::now();
    let mut rebaselined = Vec::new();
    for phrase in corpus::KO_PHRASES.iter().chain(corpus::EN_PHRASES) {
        let held = page(&format!("<p>{phrase}</p>"));
        let fresh = FreshExtraction {
            html: &held,
            url: "https://brand.example/student",
            quote: QUOTE,
            readers_agree: true,
            first_read_at: now - chrono::Duration::hours(25),
            last_read_at: now,
        };
        if fresh_anchors(&stored, &fresh).is_ok() {
            rebaselined.push(*phrase);
        }
    }
    assert!(rebaselined.is_empty(), "re-baselined: {rebaselined:?}");
}

// ── F2: the section digest moves only with the offer's own text ──────────

fn saved_page(path: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(path),
    )
    .expect("saved page")
}

/// The section digest of the element holding `quote` on a saved page, with
/// the page's visible-text hash.
fn saved_section(path: &str, quote: &str) -> (String, String) {
    let html = saved_page(path);
    let visible = crate::corporate_auto::evidence::visible_text(&html);
    let hash = crate::corporate_auto::evidence::PageEvidence::from_visible_text(&visible).hash();
    let view = PageView::new(&html, &visible);
    let fragment = split_fragments(quote).into_iter().next().unwrap();
    let i = *holders(&view, &fold_str(&fragment))
        .first()
        .unwrap_or_else(|| panic!("{path}: quote held"));
    (hash, section_digest(&view, i))
}

/// RC4 (production 2026-10-29..11-10: Office 365 A1, GitHub Copilot, Tower
/// held as "section changed" with an identical visible-text hash). Two
/// saved fetches of one page whose visible text is identical give one
/// section digest. What moved it, from these very pairs: Microsoft's
/// per-request trace id in a hidden span, docs.github.com's collapsed
/// navigation submenus, 1Password's footer language selector.
#[test]
fn identical_visible_text_gives_an_identical_section_digest() {
    let pairs: &[(&str, &str, &str)] = &[
        (
            "real_pages/4067fb7a.html",
            "page_changed_20261026/cc0aa5970d.html",
            "적격한 교육용 전자 메일 주소가 있는 교육자와 학생은 Office 365 A1을 무료로 이용할 수 있습니다.",
        ),
        (
            "real_pages/f02a666a.html",
            "page_changed_20261026/71be2fea25.html",
            "Available only to full-time university students.",
        ),
        (
            "real_pages/5b2045d2.html",
            "page_changed_20261026/b6eb33e081.html",
            "Verified students on GitHub Education get free access to Copilot's features.",
        ),
        (
            "real_pages/5b2045d2.html",
            "real_pages_changed/5b2045d2.html",
            "Verified students on GitHub Education get free access to Copilot's features.",
        ),
        (
            "real_pages/18987f71.html",
            "page_changed_20261026/87a89b26bd.html",
            "Get two years of 1Password for free with the GitHub Student Developer Pack.",
        ),
        (
            "real_pages/785fd0b2.html",
            "page_changed_20261026/33b2c968e1.html",
            "You will receive 12 months of Tower for free.",
        ),
        (
            "real_pages/5fa3af29.html",
            "real_pages_changed/5fa3af29.html",
            "학생 인증 후 10% 할인 혜택을 제공해 드립니다.",
        ),
    ];
    let mut moved = Vec::new();
    for (a, b, quote) in pairs {
        let (hash_a, section_a) = saved_section(a, quote);
        let (hash_b, section_b) = saved_section(b, quote);
        if hash_a == hash_b && section_a != section_b {
            moved.push(format!("{a} vs {b}"));
        }
    }
    assert!(
        moved.is_empty(),
        "section digest moved on identical visible text: {moved:?}"
    );
}

/// The other direction on the same saved pages: where the offer's own
/// visible text changed (Notion's "unlimited" became "multiple" members),
/// the section digest changes too.
#[test]
fn a_changed_offer_text_still_changes_the_section_digest() {
    let quote = "Your school is an accredited institution recognized by and listed in the World Higher Education Database.";
    let (hash_a, section_a) = saved_section("real_pages/4501ce12.html", quote);
    let (hash_b, section_b) = saved_section("page_changed_20261026/b93a3f1f15.html", quote);
    assert_ne!(hash_a, hash_b);
    assert_ne!(section_a, section_b);
}

/// Nothing that states the offer is loosened: a condition added in a hidden
/// note, a tooltip, a tab's state, a cell's span, and anything added at
/// body level outside `<main>` (a top bar, a popup, a hero line: the r6
/// corpus) still moves the digest; navigation, the footer and a
/// per-request token do not.
#[test]
fn chrome_and_request_tokens_fold_away_and_the_offers_text_still_counts() {
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let page = |main_extra: &str, nav_extra: &str, token: &str, body_extra: &str| {
        format!(
            "<html><body><span style='display:none'><script>x=1</script>This is the Trace Id: {token}</span>\
             <nav><ul><li><a href='/a'>홈</a></li>{nav_extra}</ul></nav>{body_extra}\
             <main><h1>학생 할인</h1><p>{quote}</p><p>대상: 대학생</p>{main_extra}</main>\
             <footer><p>고객센터</p>{nav_extra}</footer></body></html>"
        )
    };
    let digest = |html: String| {
        let visible = crate::corporate_auto::evidence::visible_text(&html);
        let view = PageView::new(&html, &visible);
        let i = holders(&view, &fold_str(quote))[0];
        section_digest(&view, i)
    };
    let token_a = "dd1ae39f4054cd6b77e732b5c82961ef";
    let token_b = "78188d8b050284289b261871e69a2db6";
    let base = digest(page("", "", token_a, ""));
    assert_eq!(base, digest(page("", "", token_b, "")), "a request token");
    assert_eq!(
        base,
        digest(page("", "<li><a href='/b'>새 메뉴</a></li>", token_a, "")),
        "navigation and footer"
    );
    for (what, main_extra, body_extra) in [
        ("hidden condition", "<p hidden>재학생 한정</p>", ""),
        ("tooltip", "<p title='재학생 한정'>대상</p>", ""),
        (
            "selected tab",
            "<div role='tab' aria-selected='true'>학생</div>",
            "",
        ),
        (
            "cell span",
            "<table><tr><td colspan='2'>대학생</td></tr></table>",
            "",
        ),
        ("new condition", "<p>단, 재학생 한정</p>", ""),
        (
            "top bar outside main",
            "",
            "<div class='top-notice'><p>학생 할인 이벤트가 종료되었습니다.</p></div>",
        ),
        (
            "a number in hidden text",
            "",
            "<span hidden>12345678901234567890123456789012345</span>",
        ),
    ] {
        assert_ne!(
            base,
            digest(page(main_extra, "", token_a, body_extra)),
            "{what} counts"
        );
    }
}

/// Review A-H1: on a one-offer page, an ending or a new condition written
/// in the footer, an aside, a sidebar or the navigation is the offer's text
/// a reader can see. A confirmed fact re-read with one such sentence added
/// is never confirmed (the five rows the review probed); a visible menu
/// link added to the navigation still is.
#[test]
fn a_visible_change_in_page_chrome_holds_the_fact() {
    let page = |main_extra: &str, footer: &str, aside: &str, nav: &str| {
        format!(
            "<html><head><title>t</title></head><body><nav><a href=\"/\">홈</a>{nav}</nav>\
             <main><h1>학생 할인</h1><p>{QUOTE}</p>\
             <p>재학 중인 대학생이라면 누구나 신청할 수 있으며 학교 이메일로 인증합니다.</p>{main_extra}</main>\
             {aside}<footer><p>고객센터 1588-0000</p>{footer}</footer></body></html>"
        )
    };
    let other = "<aside><p>다른 소식</p></aside>";
    let base = page("", "", other, "");
    let fact = confirm_on(&base, QUOTE, no_ages()).expect("confirms on the base page");
    assert!(resolve_on(&fact, &base).confirms(), "unchanged page");
    assert!(
        resolve_on(&fact, &page("", "", other, "<a href=\"/new\">새 메뉴</a>")).confirms(),
        "a menu link"
    );
    for (what, html) in [
        (
            "ending in the footer",
            page("", "<p>※ 학생 할인 혜택은 종료되었습니다.</p>", other, ""),
        ),
        (
            "condition in the footer",
            page(
                "",
                "<p>* 본 할인은 신규 가입 고객에 한해 적용됩니다.</p>",
                other,
                "",
            ),
        ),
        (
            "ending in an aside",
            page(
                "",
                "",
                "<aside><p>다른 소식</p><p>학생 할인 혜택은 종료되었습니다.</p></aside>",
                "",
            ),
        ),
        (
            "ending in a sidebar",
            page(
                "",
                "",
                "<div class=\"sidebar\"><p>학생 할인 혜택은 종료되었습니다.</p></div>",
                "",
            ),
        ),
        (
            "condition in the navigation",
            page(
                "",
                "",
                other,
                "<span>학생 할인은 신규 가입 고객에 한해 적용됩니다</span>",
            ),
        ),
    ] {
        assert!(!resolve_on(&fact, &html).confirms(), "{what}");
    }
}

/// Review A-H1: the whole-page ending scan reads an aside block that names
/// this offer with an ending (a reread republishes through it); another
/// event's ending there still says nothing.
#[test]
fn an_aside_naming_the_offer_with_an_ending_is_read_page_wide() {
    let with = |outside: &str| {
        shell(&format!(
            "<section><h2>학생 할인</h2><p>{QUOTE}</p></section>{outside}"
        ))
    };
    for outside in [
        "<footer><p>학생 할인 혜택은 종료되었습니다.</p></footer>",
        "<aside><p>학생 할인 이벤트가 종료되었습니다.</p></aside>",
    ] {
        assert!(!adopted_on(&with(outside), QUOTE).confirms(), "{outside}");
    }
    assert!(adopted_on(
        &with("<footer><p>여름 이벤트가 종료되었습니다.</p></footer>"),
        QUOTE
    )
    .confirms());
}
