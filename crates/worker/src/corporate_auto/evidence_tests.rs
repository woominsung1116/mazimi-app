use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use std::sync::{Arc, Mutex};

/// The database-backed tests share the newest census run and the loopback
/// host, so they run one at a time.
static DB_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";

fn page(body: &str) -> String {
    format!("<html><head><title>t</title><script>var price='99%';</script></head><body><main>{body}</main><footer>등록일 2026.10.01 조회 123</footer></body></html>")
}

const NIKE_QUOTE: &str = "학생 인증 후 10% 할인 혜택을 제공해 드립니다. / 30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.";

fn nike_page(percent: &str, renewal: &str, neighbour: &str) -> String {
    page(&format!(
        "<h2>학생 할인</h2><p>학생 인증 후 {percent}% 할인 혜택을 제공해 드립니다.</p>\
         <p>{renewal}일마다 인증 갱신 시, 할인 쿠폰이 <b>재발급</b>됩니다.</p><p>대상 상품 최대 {neighbour}개월</p>"
    ))
}

/// One canonical text for quotes and pages: NFKC, whitespace and NBSP
/// collapsed, zero-width marks dropped, quotes and dashes unified, Latin
/// letters case-folded. Punctuation and compatibility digits stay as
/// written.
#[test]
fn quotes_match_the_page_under_one_canonical_text() {
    let page = PageEvidence::from_visible_text(
        "Verified students get free access to Copilot\u{2019}s features \u{2014} FREE\u{a0}for\u{200b} students. \u{FF33}tudents\u{2010}only.",
    );
    for quote in [
        "Verified students get free access to Copilot's features - free for students.",
        "verified STUDENTS get free access to copilot\u{2019}s features",
        "Students-only.",
    ] {
        assert!(page.contains(quote), "{quote}");
    }
    // Punctuation is text: a dropped period is another sentence.
    assert!(
        !PageEvidence::from_visible_text("Free GitHub Pro while you are a student")
            .contains("Free GitHub Pro while you are a student.")
    );
    // A circled number is no digit: it never becomes a value.
    assert_eq!(
        PageEvidence::from_visible_text("\u{2460}2년 무료").window_signature("무료"),
        Some(value_signature("\u{2460}2년 무료"))
    );
    assert_eq!(canonical("\u{2460} \u{FF11}0%"), "\u{2460} 10%");
}

fn no_ages() -> Ages {
    Ages {
        min: None,
        max: None,
    }
}

// ── pure: text, fragments, signature ──────────────────────────────────────

#[test]
fn visible_text_drops_scripts_and_hidden_text() {
    let html = r#"<html><body><p>보이는 문장</p><script>숨은 스크립트</script>
        <div hidden>숨김 속성</div><div aria-hidden="true">아리아 숨김</div>
        <span style="display: none">인라인 숨김</span><span style="visibility:hidden">가시성 숨김</span>
        <template>템플릿</template><noscript>노스크립트</noscript>
        <span class="blind">네이버 블라인드</span><span class="screen_out">카카오 스크린아웃</span>
        <p class="text d-none">부트스트랩 숨김</p><p class="sr-only">스크린리더 전용</p>
        <div inert>비활성 영역</div><span class="hide">숨김 클래스</span>
        <span style="opacity:0">투명</span><input type="hidden" value="숨은 값">
        <p class="hidden-note">클래스 이름 일부만 같음</p></body></html>"#;
    let text = visible_text(html);
    assert!(text.contains("보이는 문장"));
    for hidden in [
        "숨은 스크립트",
        "숨김 속성",
        "아리아 숨김",
        "인라인 숨김",
        "가시성 숨김",
        "템플릿",
        "노스크립트",
        "네이버 블라인드",
        "카카오 스크린아웃",
        "부트스트랩 숨김",
        "스크린리더 전용",
        "비활성 영역",
        "숨김 클래스",
        "투명",
        "숨은 값",
    ] {
        assert!(!text.contains(hidden), "{hidden} leaked into {text:?}");
    }
    // Only whole class tokens hide text.
    assert!(text.contains("클래스 이름 일부만 같음"));
    // Tailwind: hidden on phones but shown from a breakpoint up is visible.
    let responsive = visible_text(
        "<html><body><p class=\"hidden md:block\">데스크톱 문장</p><p class=\"hidden\">숨은 문장</p><p class=\"md:hidden\">모바일 문장</p></body></html>",
    );
    assert!(responsive.contains("데스크톱 문장"));
    assert!(!responsive.contains("숨은 문장"));
    assert!(responsive.contains("모바일 문장"));
}

/// Review: a quote never matches inside a longer number.
#[test]
fn a_quote_never_matches_inside_a_longer_number() {
    let evidence = PageEvidence::from_html(&page(
        "<p>15,000원 할인 쿠폰을 드립니다.</p><p>최대 1.5% 적립</p>",
    ));
    assert!(!evidence.contains("5,000원 할인 쿠폰을 드립니다."));
    assert!(!evidence.contains("5% 적립"));
    assert!(evidence.contains("15,000원 할인 쿠폰을 드립니다."));
    assert_eq!(
        evaluate_fact(
            &evidence,
            "5,000원 할인 쿠폰을 드립니다.",
            None,
            no_ages(),
            true
        ),
        FactOutcome::QuoteMissing
    );
    let cut = PageEvidence::from_html(&page("<p>월 6,600원까지 할인</p>"));
    assert!(!cut.contains("월 6,60"));
}

#[test]
fn audit_quotes_split_into_verbatim_fragments() {
    assert_eq!(
        split_fragments(NIKE_QUOTE),
        vec![
            "학생 인증 후 10% 할인 혜택을 제공해 드립니다.",
            "30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다."
        ]
    );
    assert_eq!(
        split_fragments("휴학생 및 졸업생은 제공 대상에서 제외됩니다. ... 서비스 제공 기간: 2027년 6월 23일까지").len(),
        2
    );
    assert_eq!(
        split_fragments("Free GitHub Pro while you are a student.").len(),
        1
    );
}

#[test]
fn value_signature_reads_unit_bearing_numbers() {
    assert_eq!(
        value_signature("학생10%할인,월6,600원,최대48개월"),
        "10%,48개월,6600원"
    );
    assert_eq!(value_signature("no numbers here"), "");
}

#[test]
fn won_amounts_read_the_same_in_every_written_form() {
    for (text, expected) in [
        ("월 1만 원 할인", "10000원"),
        ("월 1만원 할인", "10000원"),
        ("월 10,000원 할인", "10000원"),
        ("1만 5천원 할인", "15000원"),
        ("1만 5,000원 할인", "15000원"),
        ("1만5천원", "15000원"),
        ("5천 원", "5000원"),
        ("1.5만원", "15000원"),
        ("１０％ 할인", "10%"),
    ] {
        assert_eq!(value_signature(text), expected, "{text}");
        assert_eq!(
            crate::corporate_auto::resilience::values_b(text),
            expected,
            "second extractor: {text}"
        );
    }
    assert_ne!(value_signature("1만 5천원"), value_signature("1만 8천원"));
}

#[test]
fn a_public_value_is_rewritten_only_where_it_can_be_read() {
    let cases = [
        (
            "학생 1만 원 할인",
            "10000원",
            "20000원",
            Ok(Some("학생 2만 원 할인")),
        ),
        (
            "학생 월 1만원 할인",
            "10000원",
            "20000원",
            Ok(Some("학생 월 2만원 할인")),
        ),
        (
            "월 10,000원 할인",
            "10000원",
            "15000원",
            Ok(Some("월 15,000원 할인")),
        ),
        (
            "1만 5천원 할인",
            "15000원",
            "18000원",
            Ok(Some("1만 8천원 할인")),
        ),
        ("학생 10% 할인", "10%", "15%", Ok(Some("학생 15% 할인"))),
        ("학생 할인", "10%", "15%", Ok(None)),
    ];
    for (text, old, new, expected) in cases {
        assert_eq!(
            rewrite_value(text, old, new).as_ref().map(|v| v.as_deref()),
            expected.as_ref().map(|v| *v),
            "{text}"
        );
    }
    for (text, old, new) in [
        ("학생 1만 할인", "10000원", "20000원"),
        ("10000 포인트 또는 1만원", "10000원", "20000원"),
        ("1만원 + 1만원", "10000원", "20000원"),
        ("학생 1만 원 할인", "10000원", "25000원"),
        ("학생 10% 할인", "10%", "3000원"),
    ] {
        assert!(
            rewrite_value(text, old, new).is_err(),
            "{text}: {old} -> {new}"
        );
    }
}

#[test]
fn presence_ignores_whitespace_and_inline_markup_only() {
    let evidence = PageEvidence::from_html(&nike_page("10", "30", "12"));
    assert!(evidence.contains("학생 인증 후 10% 할인 혜택을 제공해 드립니다."));
    assert!(evidence.contains("할인 쿠폰이 재발급됩니다."));
    assert!(!evidence.contains("학생 인증 후 15% 할인 혜택을 제공해 드립니다."));
    // Script text never counts as evidence.
    assert!(!evidence.contains("var price='99%'"));
}

// ── pure: fact decisions ──────────────────────────────────────────────────

#[test]
fn evidence_still_present_adopts_then_confirms() {
    let evidence = PageEvidence::from_html(&nike_page("10", "30", "12"));
    let FactOutcome::Confirmed {
        window_signature,
        adopted,
    } = evaluate_fact(&evidence, NIKE_QUOTE, None, no_ages(), true)
    else {
        panic!("expected adoption");
    };
    assert!(adopted);
    assert!(window_signature.contains("10%"));
    assert_eq!(
        evaluate_fact(
            &evidence,
            NIKE_QUOTE,
            Some(&window_signature),
            no_ages(),
            false
        ),
        FactOutcome::Confirmed {
            window_signature: window_signature.clone(),
            adopted: false
        }
    );
}

#[test]
fn removed_or_changed_evidence_hides() {
    let baseline = PageEvidence::from_html(&nike_page("10", "30", "12"))
        .window_signature(NIKE_QUOTE)
        .unwrap();
    // The quoted value changed: the quote is gone.
    let changed = PageEvidence::from_html(&nike_page("15", "30", "12"));
    assert_eq!(
        evaluate_fact(&changed, NIKE_QUOTE, Some(&baseline), no_ages(), true),
        FactOutcome::QuoteMissing
    );
    // The second fragment changed.
    let renewal = PageEvidence::from_html(&nike_page("10", "60", "12"));
    assert_eq!(
        evaluate_fact(&renewal, NIKE_QUOTE, Some(&baseline), no_ages(), true),
        FactOutcome::QuoteMissing
    );
    // The offer was removed.
    let removed = PageEvidence::from_html(&page("<p>이벤트가 종료되었습니다.</p>"));
    assert_eq!(
        evaluate_fact(&removed, NIKE_QUOTE, Some(&baseline), no_ages(), true),
        FactOutcome::QuoteMissing
    );
    // A value next to the quote changed: the quote is still there, but the
    // parser no longer reads the same neighbourhood.
    let neighbour = PageEvidence::from_html(&nike_page("10", "30", "24"));
    assert_eq!(
        evaluate_fact(&neighbour, NIKE_QUOTE, Some(&baseline), no_ages(), true),
        FactOutcome::WindowChanged
    );
}

#[test]
fn volatile_counters_near_the_quote_do_not_hide() {
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let a = PageEvidence::from_html(&page(&format!(
        "<p>{quote}</p><p>조회 12 등록일 2026.10.01</p>"
    )));
    let b = PageEvidence::from_html(&page(&format!(
        "<p>{quote}</p><p>조회 98 등록일 2026.10.15</p>"
    )));
    let baseline = a.window_signature(quote).unwrap();
    assert!(matches!(
        evaluate_fact(&b, quote, Some(&baseline), no_ages(), false),
        FactOutcome::Confirmed { .. }
    ));
}

#[test]
fn hidden_copy_of_the_quote_is_not_evidence() {
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let evidence = PageEvidence::from_html(&page(&format!(
        "<div style=\"display:none\">{quote}</div><p>종료</p>"
    )));
    assert_eq!(
        evaluate_fact(&evidence, quote, None, no_ages(), true),
        FactOutcome::QuoteMissing
    );
}

#[test]
fn parser_disagreement_with_the_stored_age_hides() {
    // KT Y덤 before the data fix: the page's base audience has no lower bound.
    let quote = "만 34세 이하 고객 중 아래 요금제 가입 고객";
    let evidence = PageEvidence::from_html(&page(&format!("<p>{quote}</p>")));
    assert!(matches!(
        evaluate_fact(
            &evidence,
            quote,
            None,
            Ages {
                min: Some(19),
                max: Some(34)
            },
            true
        ),
        FactOutcome::FieldMismatch(_)
    ));
    assert!(matches!(
        evaluate_fact(
            &evidence,
            quote,
            None,
            Ages {
                min: None,
                max: Some(34)
            },
            true
        ),
        FactOutcome::Confirmed { .. }
    ));
}

#[test]
fn a_removed_age_quote_is_missing_not_a_field_mismatch() {
    let quote = "만 34세 이하 고객 중 아래 요금제 가입 고객";
    let evidence = PageEvidence::from_html(&page("<p>혜택이 종료되었습니다.</p>"));
    assert_eq!(
        evaluate_fact(
            &evidence,
            quote,
            None,
            Ages {
                min: Some(19),
                max: Some(34)
            },
            true
        ),
        FactOutcome::QuoteMissing
    );
}

#[test]
fn adoption_closes_after_the_audit_window() {
    let evidence = PageEvidence::from_html(&nike_page("10", "30", "12"));
    assert_eq!(
        evaluate_fact(&evidence, NIKE_QUOTE, None, no_ages(), false),
        FactOutcome::AdoptionExpired
    );
    assert_eq!(audit_adopt_before().date_naive().to_string(), "2026-12-15");
}

// ── verbatim validator and the data-fix spec ──────────────────────────────

#[test]
fn verbatim_validator_accepts_only_audit_page_text() {
    let quote = "만 34세 이하 고객 중 아래 요금제 가입 고객";
    let says = "페이지 기본 대상은 '만 34세 이하 고객 중 아래 요금제 가입 고객'(하한 없음). '만 19세~34세'는 친구 결합 혜택에만 해당.";
    assert!(validate_anchor("만 34세 이하 고객", quote, says).is_ok());
    assert!(validate_anchor("만 19세~34세", quote, says).is_ok());
    // Auditor prose outside quotes is not page text.
    assert!(validate_anchor("친구 결합 혜택에만 해당", quote, says).is_err());
    assert!(validate_anchor("만 20세~34세", quote, says).is_err());
    assert!(validate_anchor(" ", quote, says).is_err());
}

#[test]
fn replacement_numbers_and_ages_must_come_from_the_anchor() {
    let anchor = "만 34세 이하 고객 중 아래 요금제 가입 고객";
    assert!(validate_replacement("만 34세 이하 고객이", anchor).is_ok());
    assert!(validate_replacement("만 35세 이하 고객이", anchor).is_err());
    assert!(validate_replacement("만 19~34세 고객이", anchor).is_err());
    assert!(validate_replacement("만 34세 미만 고객이", anchor).is_err());
    assert!(validate_replacement(
        "친구 결합 혜택은 만 19세~34세 고객에게 적용됩니다.",
        "만 19세~34세"
    )
    .is_ok());
    assert!(validate_replacement(
        "친구 결합 혜택은 만 18세~34세 고객에게 적용됩니다.",
        "만 19세~34세"
    )
    .is_err());
    // Numbers without an age are checked too.
    assert!(validate_replacement("데이터 최대 3배", "데이터 최대 2배").is_err());
    assert!(validate_replacement("데이터 최대 2배", "데이터 최대 2배 제공").is_ok());
    assert!(validate_age_fix(anchor, None, Some(34)).is_ok());
    assert!(validate_age_fix(anchor, Some(19), Some(34)).is_err());
    assert!(validate_age_fix(anchor, None, Some(35)).is_err());
}

fn data_fix_spec() -> Vec<Value> {
    let sql =
        include_str!("../../../../infra/migrations/20261018000002_corporate_audit_data_fixes.sql");
    let start = sql.find("$spec$").expect("spec start") + "$spec$".len();
    let end = start + sql[start..].find("$spec$").expect("spec end");
    serde_json::from_str(&sql[start..end]).expect("spec is JSON")
}

/// Runs the Rust verbatim validator over one fix of the migration spec.
fn validate_fix(fix: &Value) -> Result<(), String> {
    let id: Uuid = fix["audit_id"].as_str().unwrap().parse().unwrap();
    let item = audit()
        .items
        .iter()
        .find(|i| i.id == id)
        .ok_or("audit item missing")?;
    let quote = item.quote.clone().unwrap_or_default();
    let says = item.page_says.clone().unwrap_or_default();
    if fix["audit_quote"].as_str() != Some(quote.as_str())
        || fix["audit_page_says"].as_str() != Some(says.as_str())
    {
        return Err("embedded audit text drifted from the audit file".into());
    }
    if fix["title"].as_str() != Some(item.title.as_str()) {
        return Err("title differs from the audit".into());
    }
    match fix["kind"].as_str().unwrap() {
        "scope_campus_agnostic" => {
            let old = fix["old_name"].as_str().unwrap();
            let new = fix["new_name"].as_str().unwrap();
            if item.verdict != "WRONG" || !item.fields_wrong.iter().any(|f| f == "school_scope") {
                return Err("audit does not mark school_scope wrong".into());
            }
            if !says.contains(fix["anchor"].as_str().unwrap())
                || fix["anchor"].as_str() != Some("캠퍼스 제한 없음")
            {
                return Err("audit does not state the page has no campus restriction".into());
            }
            validate_anchor(old, &quote, &says)?;
            if item.school_scope != json!([old]) {
                return Err("old label is not what the audit saw".into());
            }
            let stripped = old
                .rsplit_once(' ')
                .filter(|(_, campus)| campus.ends_with("캠퍼스"))
                .map(|(name, _)| name);
            if stripped != Some(new) || !digits_in(new).is_empty() {
                return Err("new label is not the old label without its campus".into());
            }
        }
        "age_and_summary" => {
            if item.verdict != "WRONG" || !item.fields_wrong.iter().any(|f| f == "min_age") {
                return Err("audit does not mark min_age wrong".into());
            }
            if fix["expect"]["min_age"].as_i64().map(|v| v as i32) != item.min_age
                || fix["expect"]["max_age"].as_i64().map(|v| v as i32) != item.max_age
            {
                return Err("expected values are not what the audit saw".into());
            }
            let anchor = fix["anchor"].as_str().unwrap();
            validate_anchor(anchor, &quote, &says)?;
            validate_age_fix(
                anchor,
                fix["set"]["min_age"].as_i64().map(|v| v as i32),
                fix["set"]["max_age"].as_i64().map(|v| v as i32),
            )?;
            for replacement in fix["replacements"].as_array().unwrap() {
                let anchor = replacement["anchor"].as_str().unwrap();
                validate_anchor(anchor, &quote, &says)?;
                validate_replacement(replacement["to"].as_str().unwrap(), anchor)?;
            }
        }
        "retire_gone" => {
            if item.verdict != "PAGE_GONE" || !matches!(item.http.as_deref(), Some("404" | "410")) {
                return Err("audit does not record a gone page".into());
            }
        }
        other => return Err(format!("unknown fix kind {other}")),
    }
    Ok(())
}

#[test]
fn data_fix_spec_passes_the_verbatim_validator() {
    let spec = data_fix_spec();
    for fix in &spec {
        validate_fix(fix).unwrap_or_else(|e| panic!("{}: {e}", fix["key"]));
    }
    // Every WRONG and PAGE_GONE audit item has exactly one fix.
    let fixed: Vec<String> = spec
        .iter()
        .map(|f| f["audit_id"].as_str().unwrap().to_string())
        .collect();
    for item in audit()
        .items
        .iter()
        .filter(|i| matches!(i.verdict.as_str(), "WRONG" | "PAGE_GONE"))
    {
        assert_eq!(
            fixed
                .iter()
                .filter(|id| **id == item.id.to_string())
                .count(),
            1,
            "{}",
            item.title
        );
    }
}

#[test]
fn mutated_data_fixes_are_rejected() {
    let spec = data_fix_spec();
    let kt = spec
        .iter()
        .find(|f| f["key"] == "kt_ybonus_age")
        .unwrap()
        .clone();
    let mut wrong_age = kt.clone();
    wrong_age["set"]["min_age"] = json!(19);
    assert!(validate_fix(&wrong_age).is_err());
    let mut wrong_text = kt.clone();
    wrong_text["replacements"][0]["to"] = json!("만 35세 이하 고객이");
    assert!(validate_fix(&wrong_text).is_err());
    let mut prose_anchor = kt.clone();
    prose_anchor["replacements"][1]["anchor"] = json!("친구 결합 혜택에만 해당");
    assert!(validate_fix(&prose_anchor).is_err());
    let mut drifted = kt;
    drifted["audit_quote"] = json!("만 19세 이상 34세 이하");
    assert!(validate_fix(&drifted).is_err());
    let daegu = spec
        .iter()
        .find(|f| f["key"] == "daegu_school_scope")
        .unwrap()
        .clone();
    let mut other_school = daegu.clone();
    other_school["new_name"] = json!("대구가톨릭대학교");
    assert!(validate_fix(&other_school).is_err());
    let gone = spec
        .iter()
        .find(|f| f["key"] == "gongcha_gone")
        .unwrap()
        .clone();
    let mut not_gone = gone;
    not_gone["audit_id"] = json!("5fa3af29-0000-0000-0000-000000000000");
    assert!(validate_fix(&not_gone).is_err());
}

#[test]
fn audit_fixture_matches_the_baseline_counts() {
    let items = &audit().items;
    assert_eq!(items.len(), 87);
    let count = |v: &str| items.iter().filter(|i| i.verdict == v).count();
    assert_eq!(
        (
            count("CORRECT"),
            count("WRONG"),
            count("PAGE_GONE"),
            count("UNVERIFIABLE")
        ),
        (44, 3, 1, 39)
    );
    let with_evidence = items.iter().filter(|i| audit_fragments(i).is_ok()).count();
    assert!(
        with_evidence >= 45,
        "only {with_evidence} audit items carry usable evidence"
    );
    let correct_hidden = items
        .iter()
        .filter(|i| i.verdict == "CORRECT" && !i.visible)
        .count();
    assert_eq!(correct_hidden, 15);
}

// ── database + mock official site: the live and shadow loop ──────────────

#[derive(Clone)]
struct Site {
    robots: Arc<Mutex<(u16, String)>>,
    page: Arc<Mutex<(u16, String)>>,
    user_agents: Arc<Mutex<Vec<String>>>,
    /// Any other path (and an override of `/benefit`): status, body and an
    /// optional Location header.
    extra: Arc<Mutex<std::collections::HashMap<String, Route>>>,
    /// Requests that reached `/benefit`.
    page_hits: Arc<std::sync::atomic::AtomicUsize>,
}

type Route = (u16, String, Option<String>);

fn respond(route: Route) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (status, body, location) = route;
    let mut response = (
        axum::http::StatusCode::from_u16(status).unwrap(),
        [("content-type", "text/html; charset=utf-8")],
        body,
    )
        .into_response();
    if let Some(location) = location {
        response
            .headers_mut()
            .insert("location", location.parse().unwrap());
    }
    response
}

async fn serve(site: Site) -> String {
    use axum::{http::HeaderMap, response::IntoResponse, routing::get, Router};
    let robots_site = site.clone();
    let page_site = site.clone();
    let app = Router::new()
        .route(
            "/robots.txt",
            get(move |headers: HeaderMap| {
                let site = robots_site.clone();
                async move {
                    site.user_agents
                        .lock()
                        .unwrap()
                        .push(headers["user-agent"].to_str().unwrap().to_string());
                    let (status, body) = site.robots.lock().unwrap().clone();
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "text/plain")],
                        body,
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/benefit",
            get(move |headers: HeaderMap| {
                let site = page_site.clone();
                async move {
                    site.page_hits
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    site.user_agents
                        .lock()
                        .unwrap()
                        .push(headers["user-agent"].to_str().unwrap().to_string());
                    if let Some(route) = site.extra.lock().unwrap().get("/benefit").cloned() {
                        return respond(route);
                    }
                    let (status, body) = site.page.lock().unwrap().clone();
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [
                            ("content-type", "text/html; charset=utf-8"),
                            ("retry-after", "3600"),
                        ],
                        body,
                    )
                        .into_response()
                }
            }),
        )
        .fallback({
            let site = site.clone();
            move |request: axum::extract::Request| {
                let site = site.clone();
                async move {
                    let path = request.uri().path().to_string();
                    match site.extra.lock().unwrap().get(&path).cloned() {
                        Some(route) => respond(route),
                        None => respond((404, "missing".into(), None)),
                    }
                }
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/benefit")
}

struct Fixture {
    candidate_id: Uuid,
    program_id: Uuid,
    domain_id: Uuid,
}

async fn fixture(pool: &PgPool, url: &str) -> Fixture {
    let domain_id = Uuid::new_v4();
    let candidate_id = Uuid::new_v4();
    let program_id = Uuid::new_v4();
    let domain = format!("{}.evidence.example", domain_id.simple());
    sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
                 VALUES($1,'evidence fixture',$2,'brand_official','verified','fixture',now(),'pending','manual_review')")
        .bind(domain_id).bind(&domain).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at)
                 VALUES($1,$2,$3,$3 || '#' || $1::text,'manual','verification_due','detail',now())")
        .bind(candidate_id).bind(domain_id).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                 VALUES($1,'corporate_benefit','corporate_official',$2,'증거 픽스처 학생 할인','학생 10% 할인','픽스처',$3,'review_required','corporate','discount',false,'review_due',ARRAY[]::text[])")
        .bind(program_id).bind(format!("fixture:{program_id}")).bind(url).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verification_state,verified_at,review_due_at,verification_mode)
                 VALUES($1,$2,'review_due',now()-interval '60 days',now()-interval '30 days','evidence_auto')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    // Hidden by the overdue sweep while it was published.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'verification_overdue',true,'always_open')")
        .bind(program_id).bind(candidate_id).execute(pool).await.unwrap();
    for fragment in split_fragments(NIKE_QUOTE) {
        let (fact_type, value) = read_quote(&fragment);
        sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,adopt_before)
                     VALUES($1,$2,$3,$4,$5,'fixture',now()+interval '1 day')")
            .bind(program_id).bind(fact_type).bind(value).bind(&fragment).bind(quote_key(program_id, &fragment))
            .execute(pool).await.unwrap();
    }
    let run_id: Uuid = sqlx::query_scalar("INSERT INTO corporate_census_runs(status,finished_at) VALUES('completed',now()) RETURNING id")
        .fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
                 VALUES($1,'monitor',$2,$3,$4,$5,'direct','direct_html',true,'ok','none')")
        .bind(run_id).bind(domain_id).bind(candidate_id).bind(program_id).bind(url).execute(pool).await.unwrap();
    operations::record_event(
        pool,
        &operations::EventCommand {
            fingerprint: &format!("source_review:{candidate_id}"),
            severity: operations::Severity::Error,
            component: "source_monitoring",
            event_type: "source_verification_overdue",
            subject_type: Some("benefit_source_candidate"),
            subject_id: Some(&candidate_id.to_string()),
            message: "An official benefit source exceeded its verification deadline",
            details: json!({"programs_held": 1}),
        },
    )
    .await
    .unwrap();
    Fixture {
        candidate_id,
        program_id,
        domain_id,
    }
}

async fn state(pool: &PgPool, f: &Fixture) -> (bool, String, String, i64, Option<String>, String) {
    let (active, status): (bool, String) =
        sqlx::query_as("SELECT is_active, program_status FROM programs WHERE id=$1")
            .bind(f.program_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let monitor: String = sqlx::query_scalar(
        "SELECT verification_state FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let open_holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_source_holds WHERE program_id=$1 AND resolved_at IS NULL",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let event: (String, Option<String>) = sqlx::query_as(
        "SELECT status, resolution_note FROM operational_events WHERE fingerprint=$1",
    )
    .bind(format!("source_review:{}", f.candidate_id))
    .fetch_one(pool)
    .await
    .unwrap();
    (active, status, monitor, open_holds, event.1, event.0)
}

async fn last_outcome(pool: &PgPool, f: &Fixture) -> (String, bool) {
    sqlx::query_as("SELECT outcome, applied FROM corporate_evidence_checks WHERE candidate_id=$1 ORDER BY checked_at DESC, id LIMIT 1")
        .bind(f.candidate_id).fetch_one(pool).await.unwrap()
}

async fn pass(pool: &PgPool, f: &Fixture, mode: VerifyMode) {
    sqlx::query("UPDATE corporate_evidence_targets SET next_check_at=now() WHERE candidate_id=$1")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        mode,
        &http,
        Some(vec![f.candidate_id]),
        Some(std::time::Duration::from_millis(20)),
    )
    .await
    .unwrap();
}

/// `publish`: `CORPORATE_AUTO_PUBLISH_ENABLED` for a live pass.
async fn pass_publishing(pool: &PgPool, f: &Fixture, mode: VerifyMode, publish: bool) {
    sqlx::query("UPDATE corporate_evidence_targets SET next_check_at=now() WHERE candidate_id=$1")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    let http = LoopbackHttp::new(UA);
    super::run_with_publish(
        pool,
        mode,
        publish,
        false,
        &http,
        Some(vec![f.candidate_id]),
        Some(std::time::Duration::from_millis(20)),
    )
    .await
    .unwrap();
}

/// Every public value of the programme (what publish alone may change);
/// visibility and source-health bookkeeping are left out.
async fn values(pool: &PgPool, f: &Fixture) -> Value {
    sqlx::query_scalar(
        "SELECT to_jsonb(p) - 'is_active' - 'program_status' - 'updated_at' - 'retired_at'
                - 'retirement_reason' - 'source_health' - 'source_verified_at'
                - 'source_review_due_at' - 'source_change_detected_at'
                - 'source_missing_runs' - 'source_missing_since'
         FROM programs p WHERE p.id = $1",
    )
    .bind(f.program_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn cleanup(pool: &PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM operational_events WHERE fingerprint LIKE '%' || $1::text")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(f.program_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM corporate_census_runs WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE domain_id=$1)")
        .bind(f.domain_id).execute(pool).await.unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(f.domain_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn evidence_loop_confirms_hides_restores_and_retires_on_a_mock_site() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping database-backed evidence test");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let site = Site {
        robots: Arc::new(Mutex::new((200, "User-agent: *\nAllow: /\n".into()))),
        page: Arc::new(Mutex::new((200, nike_page("10", "30", "12")))),
        user_agents: Arc::new(Mutex::new(Vec::new())),
        extra: Default::default(),
        page_hits: Default::default(),
    };
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;

    // Shadow first: the decision is logged, nothing public changes.
    pass(&pool, &f, VerifyMode::Shadow).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), false));
    let (active, _, monitor, holds, _, event) = state(&pool, &f).await;
    assert!(!active);
    assert_eq!(
        (monitor.as_str(), holds, event.as_str()),
        ("review_due", 1, "open")
    );

    // Live: evidence still present → republished, hold and event resolved.
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    let (active, status, monitor, holds, note, event) = state(&pool, &f).await;
    assert!(active, "a hidden-correct offer returns through evidence");
    assert_eq!(
        (status.as_str(), monitor.as_str(), holds, event.as_str()),
        ("always_open", "current", 0, "resolved")
    );
    assert!(note.unwrap().contains("evidence re-verified"));
    let due_days: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM review_due_at - now())/86400)::float8 FROM program_source_monitors WHERE program_id=$1")
        .bind(f.program_id).fetch_one(&pool).await.unwrap();
    assert!((13.9..=14.1).contains(&due_days));

    // A value next to the quote changed → its value window reads otherwise:
    // hidden, never auto-changed.
    site.page.lock().unwrap().1 = nike_page("10", "30", "24");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "window_changed");
    let (active, status, monitor, holds, _, _) = state(&pool, &f).await;
    assert!(!active);
    assert_eq!(
        (status.as_str(), monitor.as_str(), holds),
        ("review_required", "change_detected", 1)
    );
    let summary: String = sqlx::query_scalar("SELECT summary FROM programs WHERE id=$1")
        .bind(f.program_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(summary, "학생 10% 할인");

    // The page reverts → restored automatically.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    assert!(state(&pool, &f).await.0);

    // The quoted value changed → the quote is gone: hidden for fresh
    // extraction, nothing proposed.
    site.page.lock().unwrap().1 = nike_page("15", "30", "12");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "quote_missing");
    assert!(!state(&pool, &f).await.0);

    // Back, then robots disallows our agent: blocked, shown inside its
    // grace window (reverify), retried daily.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12");
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: MazimiBenefitBot\nDisallow: /benefit\n\nUser-agent: *\nAllow: /\n".into(),
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), false)
    );
    assert!(state(&pool, &f).await.0);

    // Robots allows us again and the page confirms.
    *site.robots.lock().unwrap() = (200, "User-agent: *\nAllow: /\n".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    assert!(state(&pool, &f).await.0);

    // 429 with Retry-After: transient, counted, not hidden on its own.
    *site.page.lock().unwrap() = (429, "slow down".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("rate_limited".into(), false)
    );
    assert!(state(&pool, &f).await.0);

    // Challenge page → blocked (A1), shown inside grace.
    *site.page.lock().unwrap() = (403, "<title>Just a moment...</title>".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("blocked".into(), false));
    assert!(state(&pool, &f).await.0);

    // 404 twice at least 24 h apart → retired as provider_withdrawn.
    *site.page.lock().unwrap() = (404, "not found".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "gone");
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at=now()-interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "retired_gone");
    let (active, status, reason): (bool, String, Option<String>) = sqlx::query_as(
        "SELECT is_active, program_status, retirement_reason FROM programs WHERE id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (active, status.as_str(), reason.as_deref()),
        (false, "closed", Some("provider_withdrawn"))
    );

    // Every request carried the honest bot user agent.
    let agents = site.user_agents.lock().unwrap().clone();
    assert!(!agents.is_empty() && agents.iter().all(|a| a == UA));
    cleanup(&pool, &f).await;
}

/// §28: live with publishing off still applies the reader: a review_due
/// offer whose evidence reads the same is restored, a changed one hidden,
/// a gone one retired. No public value is written.
#[tokio::test]
async fn live_without_publish_restores_hides_and_retires_without_writing_values() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let before = values(&pool, &f).await;

    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    let (active, status, monitor, holds, _, event) = state(&pool, &f).await;
    assert!(active, "a confirmed review_due offer is restored");
    assert_eq!(
        (status.as_str(), monitor.as_str(), holds, event.as_str()),
        ("always_open", "current", 0, "resolved")
    );
    assert_eq!(values(&pool, &f).await, before);

    site.page.lock().unwrap().1 = nike_page("10", "30", "24");
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "window_changed");
    assert!(!state(&pool, &f).await.0, "a changed value window hides");
    assert_eq!(values(&pool, &f).await, before);

    *site.page.lock().unwrap() = (404, "not found".into());
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at=now()-interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "retired_gone");
    assert!(!state(&pool, &f).await.0, "a gone page is retired");
    cleanup(&pool, &f).await;
}

/// A confirmation is a verbatim re-read of the official page under robots,
/// the opt-out check and the domain policy: it lifts revoked access and a
/// gone page as it lifts failed evidence (supersedes §28's person). Only a
/// publisher opt-out waits for its own flow.
#[tokio::test]
async fn a_confirm_lifts_every_hold_but_a_publisher_optout() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    for (reason, restored) in [
        ("access_revoked", true),
        ("not_found", true),
        ("evidence_failed", true),
        ("publisher_optout", false),
    ] {
        let site = site_with(nike_page("10", "30", "12"));
        let f = fixture(&pool, &serve(site.clone()).await).await;
        sqlx::query("UPDATE program_source_holds SET hold_reason=$2 WHERE program_id=$1 AND resolved_at IS NULL")
            .bind(f.program_id)
            .bind(reason)
            .execute(&pool)
            .await
            .unwrap();
        for publish in [false, true] {
            pass_publishing(&pool, &f, VerifyMode::Live, publish).await;
            assert_eq!(last_outcome(&pool, &f).await.0, "confirmed", "{reason}");
            let (active, _, _, holds, _, _) = state(&pool, &f).await;
            assert_eq!(active, restored, "{reason} publish={publish}");
            assert_eq!(holds, i64::from(!restored), "{reason} publish={publish}");
        }
        cleanup(&pool, &f).await;
    }
}

/// §28: a moved page is never adopted with publishing off (its URL is a
/// public value); the agreeing proposal stays for review.
#[tokio::test]
async fn live_without_publish_never_adopts_a_moved_url() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    assert!(state(&pool, &f).await.0);
    route(&site, "/benefit", 404, "missing", None);
    route(
        &site,
        "/sitemap.xml",
        200,
        &format!("<?xml version=\"1.0\"?><urlset><url><loc>{base}/student/benefit-new</loc></url></urlset>"),
        None,
    );
    route(
        &site,
        "/student/benefit-new",
        200,
        &redesigned_nike("10", "30"),
        None,
    );
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    backdate_proposals(&pool, &f).await;
    pass_publishing(&pool, &f, VerifyMode::Live, false).await;
    assert_ne!(last_outcome(&pool, &f).await.0, "url_adopted");
    let (candidate_url, official_url): (String, String) = sqlx::query_as(
        "SELECT c.url, p.official_url FROM benefit_source_candidates c, programs p WHERE c.id=$1 AND p.id=$2",
    )
    .bind(f.candidate_id)
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (candidate_url.as_str(), official_url.as_str()),
        (url.as_str(), url.as_str())
    );
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn opted_out_source_is_never_fetched_and_is_hidden() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let site = Site {
        robots: Arc::new(Mutex::new((200, "User-agent: *\nAllow: /\n".into()))),
        page: Arc::new(Mutex::new((200, nike_page("10", "30", "12")))),
        user_agents: Arc::new(Mutex::new(Vec::new())),
        extra: Default::default(),
        page_hits: Default::default(),
    };
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    let requests_before = site.user_agents.lock().unwrap().len();
    let prefix = url.trim_end_matches("benefit").to_string();
    let optout: Uuid = sqlx::query_scalar("INSERT INTO benefit_source_optouts(url_prefix,scope,channel,effective_at) VALUES($1,'both','web_form',now()) RETURNING id")
        .bind(prefix.replace("http://", "https://")).fetch_one(&pool).await.unwrap();
    let matched = policy::opted_out(
        &pool,
        &Url::parse(&url.replace("http://", "https://")).unwrap(),
    )
    .await
    .unwrap();
    assert!(matched);
    sqlx::query("DELETE FROM benefit_source_optouts WHERE id=$1")
        .bind(optout)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO benefit_source_optouts(domain,scope,channel,effective_at) VALUES('127.0.0.1','both','web_form',now())")
        .execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "opted_out");
    assert!(!state(&pool, &f).await.0);
    assert_eq!(
        site.user_agents.lock().unwrap().len(),
        requests_before,
        "no request after an opt-out"
    );
    sqlx::query("DELETE FROM benefit_source_optouts WHERE domain='127.0.0.1'")
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn data_fix_migration_applied_the_audited_values() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let kt: Option<(Option<i32>, Option<i32>, String)> = sqlx::query_as(
        "SELECT min_age, max_age, summary FROM programs WHERE source_type='corporate_official'
         AND source_id='https://product.kt.com/benefit/membership/mobile/y-bonus.html'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let Some((min, max, summary)) = kt else {
        eprintln!("seed programmes absent; skipping");
        return;
    };
    assert_eq!((min, max), (None, Some(34)));
    assert!(summary.starts_with("만 34세 이하 고객이"));
    assert!(summary.contains("친구 결합 혜택은 만 19세~34세"));
    assert!(!summary.contains("만 19~34세가"));
    let scopes: Vec<(String, Value)> = sqlx::query_as(
        "SELECT university_id, school_scope FROM programs WHERE source_id IN
         ('verified:20260923:expansion2:daegu_office365','verified:20260923:expansion2:yu_office365') ORDER BY university_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        scopes,
        vec![
            ("daegu-daegu".into(), json!(["대구대학교"])),
            ("yu-daegu".into(), json!(["영남대학교"]))
        ]
    );
    let resolved: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM universities WHERE is_active AND '대구대학교 대구캠퍼스' = ANY(aliases)",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        resolved,
        vec!["daegu-daegu".to_string()],
        "saved campus names still resolve"
    );
    let gongcha: (bool, Option<String>) = sqlx::query_as(
        "SELECT is_active, retirement_reason FROM programs WHERE source_id='https://www.gong-cha.co.kr/brand/member/membership1.php'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(gongcha, (false, Some("provider_withdrawn".into())));
    let backups: i64 = sqlx::query_scalar("SELECT count(*) FROM corporate_audit_fix_backup")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(backups, 6);
}

#[tokio::test]
async fn new_tables_are_service_only() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    for table in [
        "benefit_source_optouts",
        "corporate_census_runs",
        "corporate_census_entries",
        "corporate_offer_facts",
        "corporate_evidence_targets",
        "corporate_evidence_checks",
        "corporate_pipeline_daily",
        "corporate_change_proposals",
        "corporate_audit_fix_backup",
    ] {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname=$1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            enabled && forced,
            "{table} must ENABLE and FORCE row level security"
        );
        let policies: Vec<String> =
            sqlx::query_scalar("SELECT qual FROM pg_policies WHERE tablename=$1")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(policies, vec!["is_service_role()".to_string()], "{table}");
    }
}

#[tokio::test]
async fn seeding_is_idempotent_and_skips_unfixed_or_unverifiable_items() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let first = seed_audit_evidence(&pool).await.unwrap();
    let second = seed_audit_evidence(&pool).await.unwrap();
    assert_eq!(second.facts_inserted, 0, "re-seeding inserts nothing");
    assert_eq!(
        first
            .skipped
            .get("verdict_without_evidence")
            .copied()
            .unwrap_or(0),
        40
    );
    let gone: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts f JOIN programs p ON p.id=f.program_id WHERE p.title='공차 멤버십 스탬프 혜택'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(gone, 0);
}

#[tokio::test]
async fn events_dry_run_projects_how_open_source_events_resolve() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let site = Site {
        robots: Arc::new(Mutex::new((
            200,
            "User-agent: MazimiBenefitBot\nDisallow: /\n".into(),
        ))),
        page: Arc::new(Mutex::new((200, nike_page("10", "30", "12")))),
        user_agents: Arc::new(Mutex::new(Vec::new())),
        extra: Default::default(),
        page_hits: Default::default(),
    };
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let fingerprint = format!("source_review:{}", f.candidate_id);
    let find = |report: &Value| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["fingerprint"] == fingerprint.as_str())
            .cloned()
            .unwrap()
    };
    let before = super::super::events::dry_run_report(&pool).await.unwrap();
    assert_eq!(find(&before)["reason"], "evidence_auto, not checked yet");
    // Shadow pass while robots blocks: projected "hidden", event untouched.
    pass(&pool, &f, VerifyMode::Shadow).await;
    let report = super::super::events::dry_run_report(&pool).await.unwrap();
    assert_eq!(find(&report)["projection"], "resolve_hidden");
    assert_eq!(find(&report)["reason"], "robots_disallowed");
    assert_eq!(state(&pool, &f).await.5, "open");
    // Robots opens, still shadow: projected "confirmed".
    *site.robots.lock().unwrap() = (200, "User-agent: *\nAllow: /\n".into());
    pass(&pool, &f, VerifyMode::Shadow).await;
    let report = super::super::events::dry_run_report(&pool).await.unwrap();
    assert_eq!(find(&report)["projection"], "resolve_confirmed");
    assert!(report["counts"]["resolve_confirmed"].as_u64().unwrap() >= 1);
    // A still-blocked, already hidden source resolves with its reason once
    // and is not re-raised by the next pass.
    *site.page.lock().unwrap() = (503, "maintenance".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        state(&pool, &f).await.5,
        "resolved",
        "transient failure on a hidden source resolves as still blocked"
    );
    let note = state(&pool, &f).await.4.unwrap();
    assert!(note.contains("still blocked (fetch_failed)"), "{note}");
    pass(&pool, &f, VerifyMode::Live).await;
    let (status, count): (String, i32) = sqlx::query_as(
        "SELECT status, occurrence_count FROM operational_events WHERE fingerprint=$1",
    )
    .bind(&fingerprint)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), count), ("resolved", 1));
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn census_blocked_hidden_sources_resolve_once_and_stay_in_the_digest() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let f = fixture(
        &pool,
        "https://www.cjone.example/cjmweb/point-card/brand.do",
    )
    .await;
    // A robots-allowlist source: never evidence_auto, census says snippet.
    sqlx::query("UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE corporate_census_entries SET lane='snippet', reason='robots_disallow' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    let fingerprint = format!("source_review:{}", f.candidate_id);
    let report = super::super::events::dry_run_report(&pool).await.unwrap();
    let item = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fingerprint"] == fingerprint.as_str())
        .unwrap()
        .clone();
    assert_eq!(item["projection"], "resolve_still_blocked");
    assert_eq!(item["reason"], "census snippet:robots_disallow");

    // Shadow never touches events.
    let http = LoopbackHttp::new(UA);
    run_with(
        &pool,
        VerifyMode::Shadow,
        &http,
        Some(vec![f.candidate_id]),
        Some(std::time::Duration::from_millis(5)),
    )
    .await
    .unwrap();
    assert_eq!(state(&pool, &f).await.5, "open");
    // Live resolves it once with the census reason.
    run_with(
        &pool,
        VerifyMode::Live,
        &http,
        Some(vec![f.candidate_id]),
        Some(std::time::Duration::from_millis(5)),
    )
    .await
    .unwrap();
    let (_, _, _, _, note, status) = state(&pool, &f).await;
    assert_eq!(status, "resolved");
    assert!(note
        .unwrap()
        .contains("still blocked (census snippet:robots_disallow)"));
    // The overdue sweep does not raise it again for a held source.
    crate::source_monitoring::hold_overdue_sources(&pool, chrono::Duration::hours(1), 1_000)
        .await
        .unwrap();
    assert_eq!(state(&pool, &f).await.5, "resolved");
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert!(
        metrics["digest_census_blocked_hidden"]["snippet:robots_disallow"]
            .as_i64()
            .unwrap()
            >= 1
    );
    cleanup(&pool, &f).await;
}

// ── redesign resilience on the mock site ──────────────────────────────────

/// The same offer after a structural redesign: new wrappers, classes and
/// heading level, the same copy and headings. The neighbouring 12-month
/// value stays. (Rewritten copy is another statement: hidden, re-extracted.)
fn redesigned_nike(percent: &str, renewal: &str) -> String {
    page(&format!(
        "<section class=\"promo\"><h3 class=\"promo__title\">학생 할인</h3><div class=\"promo__text\">\
         <p>학생 인증 후 {percent}% 할인 혜택을 제공해 드립니다.</p>\
         <p>{renewal}일마다 인증 갱신 시, 할인 쿠폰이 <b>재발급</b>됩니다.</p><p>대상 상품 최대 12개월</p></div></section>"
    ))
}

/// The redesign with full-width digits: the page reads the same, the
/// statement is relocated and its verbatim text replaces the stored quote.
fn redesigned_nike_retyped(percent: &str, renewal: &str) -> String {
    redesigned_nike(percent, renewal).replace(
        &format!("<p>{renewal}일마다"),
        &format!(
            "<p>{}일마다",
            renewal
                .chars()
                .map(|c| char::from_u32(c as u32 + 0xFEE0).unwrap_or(c))
                .collect::<String>()
        ),
    )
}

async fn reextract_of(pool: &PgPool, f: &Fixture) -> Option<String> {
    sqlx::query_scalar(
        "SELECT reextract_reason FROM corporate_evidence_targets
         WHERE candidate_id=$1 AND reextract_requested_at IS NOT NULL",
    )
    .bind(f.candidate_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .flatten()
}

/// Review A-M4: a held source that rereading already republished three
/// times in 30 days is not queued for rereading again; its reason says so.
#[tokio::test]
async fn a_source_republished_too_often_is_not_queued_for_rereading_again() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0, "confirmed first");
    for _ in 0..super::VOLATILE_REPUBLISHES {
        sqlx::query(
            "INSERT INTO benefit_source_checks (candidate_id, check_kind, error_summary, review_status)
             VALUES ($1, 'unchanged', $2, 'not_required')",
        )
        .bind(f.candidate_id)
        .bind(super::super::publish::REPUBLISHED_SUMMARY)
        .execute(&pool)
        .await
        .unwrap();
    }
    *site.page.lock().unwrap() = (200, nike_page("15", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(!state(&pool, &f).await.0, "held");
    assert_eq!(reextract_of(&pool, &f).await, None, "not queued");
    let reason: Option<String> = sqlx::query_scalar(
        "SELECT reextract_reason FROM corporate_evidence_targets WHERE candidate_id=$1",
    )
    .bind(f.candidate_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reason.as_deref(), Some(super::VOLATILE_PAGE));
    cleanup(&pool, &f).await;
}

async fn value_proposals(pool: &PgPool, f: &Fixture) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM corporate_change_proposals WHERE candidate_id=$1 AND kind='value'",
    )
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn route(site: &Site, path: &str, status: u16, body: &str, location: Option<&str>) {
    site.extra.lock().unwrap().insert(
        path.into(),
        (status, body.into(), location.map(str::to_string)),
    );
}

async fn summary_of(pool: &PgPool, f: &Fixture) -> String {
    sqlx::query_scalar("SELECT summary FROM programs WHERE id=$1")
        .bind(f.program_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn backdate_proposals(pool: &PgPool, f: &Fixture) {
    sqlx::query(
        "UPDATE corporate_change_proposals
         SET first_seen_at = first_seen_at - interval '25 hours',
             first_live_at = first_live_at - interval '25 hours'
         WHERE candidate_id = $1 AND status = 'pending'",
    )
    .bind(f.candidate_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn db() -> Option<PgPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    Some(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap(),
    )
}

fn site_with(page_html: String) -> Site {
    Site {
        robots: Arc::new(Mutex::new((200, "User-agent: *\nAllow: /\n".into()))),
        page: Arc::new(Mutex::new((200, page_html))),
        user_agents: Arc::new(Mutex::new(Vec::new())),
        extra: Default::default(),
        page_hits: Default::default(),
    }
}

#[tokio::test]
async fn a_redesign_relocates_and_a_changed_value_is_never_applied() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;

    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    let digests: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts WHERE program_id=$1
         AND anchors->'fingerprint'->>'page' LIKE 'v2:%'",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        digests, 2,
        "each confirmed fact stores its element fingerprint and page digest"
    );

    // Redesign, same text: relocated, still shown, new anchors stored.
    *site.page.lock().unwrap() = (200, redesigned_nike("10", "30"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("relocated".into(), true));
    assert!(state(&pool, &f).await.0);
    let (quote, path): (String, serde_json::Value) = sqlx::query_as(
        "SELECT quote, anchors->'fingerprint'->'path' FROM corporate_offer_facts
         WHERE program_id=$1 AND quote LIKE '%할인 혜택을%'",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(quote, "학생 인증 후 10% 할인 혜택을 제공해 드립니다.");
    assert!(path.to_string().contains("section"), "{path}");
    assert_eq!(reextract_of(&pool, &f).await, None);

    // The value changes: the quote is no longer on the page, so the offer
    // hides at once and is flagged for fresh extraction. Nothing is
    // proposed or applied, however long the new value stays.
    *site.page.lock().unwrap() = (200, redesigned_nike("15", "30"));
    for _ in 0..2 {
        pass(&pool, &f, VerifyMode::Live).await;
        assert_eq!(
            last_outcome(&pool, &f).await,
            ("quote_missing".into(), true)
        );
        assert!(!state(&pool, &f).await.0);
        backdate_proposals(&pool, &f).await;
    }
    assert_eq!(summary_of(&pool, &f).await, "학생 10% 할인");
    assert_eq!(value_proposals(&pool, &f).await, 0);
    assert_eq!(
        reextract_of(&pool, &f).await.as_deref(),
        Some("quote_missing")
    );
    // The value matcher's reading is kept as a shadow metric.
    let shadow: Option<String> = sqlx::query_scalar(
        "SELECT f->'detail'->'shadow_update'->>'new' FROM corporate_evidence_checks c,
                jsonb_array_elements(c.details->'facts') f
         WHERE c.candidate_id=$1 AND f->'detail' ? 'shadow_update'
         ORDER BY c.checked_at DESC LIMIT 1",
    )
    .bind(f.candidate_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(shadow.as_deref(), Some("15%"));
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert!(
        metrics["resilience_24h"]["shadow_updates_read"]
            .as_i64()
            .unwrap()
            >= 1
    );

    // The page reads as before again: confirmed, and the flag clears.
    *site.page.lock().unwrap() = (200, redesigned_nike("10", "30"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    assert_eq!(reextract_of(&pool, &f).await, None);
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn a_moved_page_is_rediscovered_by_sitemap_and_adopted_only_after_agreement() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);

    route(&site, "/benefit", 404, "missing", None);
    route(
        &site,
        "/sitemap.xml",
        200,
        &format!("<?xml version=\"1.0\"?><urlset><url><loc>{base}/about</loc></url><url><loc>{base}/student/benefit-new</loc></url></urlset>"),
        None,
    );
    route(
        &site,
        "/about",
        200,
        &page("<p>회사 소개 페이지입니다. 설립 10년</p>"),
        None,
    );
    route(
        &site,
        "/student/benefit-new",
        200,
        &redesigned_nike("10", "30"),
        None,
    );

    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("url_move_proposed".into(), true)
    );
    assert!(
        !state(&pool, &f).await.0,
        "hidden until the new URL is adopted"
    );
    // The 404 retirement rule does not fire while the page is found again.
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at = now() - interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "url_move_proposed");

    backdate_proposals(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("url_adopted".into(), true));
    assert!(state(&pool, &f).await.0);
    let moved: String = sqlx::query_scalar("SELECT url FROM benefit_source_candidates WHERE id=$1")
        .bind(f.candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(moved, format!("{base}/student/benefit-new"));
    // From now on the new URL is read directly.
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    assert!(matches!(
        last_outcome(&pool, &f).await.0.as_str(),
        "confirmed" | "relocated"
    ));
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn a_page_that_is_gone_without_a_match_still_retires_after_the_404_rule() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    route(&site, "/benefit", 404, "missing", None);
    // The sitemap only offers a page with another value: no match.
    route(
        &site,
        "/sitemap.xml",
        200,
        &format!("<urlset><url><loc>{base}/student/other</loc></url></urlset>"),
        None,
    );
    route(
        &site,
        "/student/other",
        200,
        &redesigned_nike("50", "30"),
        None,
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "gone");
    assert!(!state(&pool, &f).await.0);
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at = now() - interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "retired_gone");
    cleanup(&pool, &f).await;
}

/// Self-healing URL: the page redirects within the site and every fact
/// confirms there. The offer stays shown meanwhile (its old link still
/// leads to it); the URL moves only after two agreeing sightings, with the
/// old and the new URL kept as the proposal's evidence.
#[tokio::test]
async fn a_same_host_redirect_waits_for_agreement_before_the_url_moves() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    route(&site, "/benefit", 301, "", Some("/students"));
    route(&site, "/students", 200, &nike_page("10", "30", "12"), None);
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("url_move_proposed".into(), true)
    );
    assert!(state(&pool, &f).await.0, "shown while the move waits");
    backdate_proposals(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("url_adopted".into(), true));
    assert!(state(&pool, &f).await.0);
    let moved = url.replace("/benefit", "/students");
    let (candidate_url, old, new): (String, String, String) = sqlx::query_as(
        "SELECT c.url, p.old_value, p.new_value FROM benefit_source_candidates c
         JOIN corporate_change_proposals p ON p.candidate_id = c.id AND p.kind = 'url' AND p.status = 'applied'
         WHERE c.id = $1",
    )
    .bind(f.candidate_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((candidate_url, old, new), (moved.clone(), url, moved));
    cleanup(&pool, &f).await;
}

/// Review R3: the hourly audit seed never re-inserts a quote it already
/// holds. A redesign that retypes the quote in full-width digits keeps the
/// same canonical quote: relocated, nothing replaced, nothing re-seeded.
#[tokio::test]
async fn the_audit_seed_never_reinserts_a_replaced_quote() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    *site.page.lock().unwrap() = (200, redesigned_nike_retyped("10", "30"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "relocated");
    let origin: Option<String> = sqlx::query_scalar(
        "SELECT anchors->>'origin_quote' FROM corporate_offer_facts WHERE program_id=$1 AND quote LIKE '%재발급%'",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(origin, None, "the same canonical quote is kept");
    let facts = |pool: PgPool, id: Uuid| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM corporate_offer_facts WHERE program_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = facts(pool.clone(), f.program_id).await;
    let later = Utc::now() + chrono::Duration::days(1);
    for replaced in split_fragments(NIKE_QUOTE) {
        assert_eq!(
            insert_seed_fragment(&pool, f.program_id, &replaced, later)
                .await
                .unwrap(),
            0,
            "{replaced} is held already"
        );
    }
    assert_eq!(facts(pool.clone(), f.program_id).await, before);
    // A fragment never seen before is still seeded.
    assert_eq!(
        insert_seed_fragment(&pool, f.program_id, "대상 상품 최대 12개월", later)
            .await
            .unwrap(),
        1
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(
        state(&pool, &f).await.0,
        "the offer stays shown after the next seed and check"
    );
    cleanup(&pool, &f).await;
}

/// A value change seen in shadow and then live is never applied in either.
#[tokio::test]
async fn a_changed_value_is_never_applied_in_shadow_or_live() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    *site.page.lock().unwrap() = (200, nike_page("15", "30", "12"));
    for mode in [
        VerifyMode::Shadow,
        VerifyMode::Shadow,
        VerifyMode::Live,
        VerifyMode::Live,
    ] {
        pass(&pool, &f, mode).await;
        backdate_proposals(&pool, &f).await;
        assert_eq!(last_outcome(&pool, &f).await.0, "quote_missing");
    }
    assert!(!state(&pool, &f).await.0);
    assert_eq!(summary_of(&pool, &f).await, "학생 10% 할인");
    assert_eq!(value_proposals(&pool, &f).await, 0);
    cleanup(&pool, &f).await;
}

/// Review N1: after a rollback from live to shadow, a changed value is not
/// applied and a moved URL is not adopted in shadow.
#[tokio::test]
async fn shadow_never_applies_a_change_even_after_live_agreement() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    *site.page.lock().unwrap() = (200, nike_page("15", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    backdate_proposals(&pool, &f).await;
    // Rolled back to shadow: nothing is applied.
    pass(&pool, &f, VerifyMode::Shadow).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("quote_missing".into(), false)
    );
    assert_eq!(summary_of(&pool, &f).await, "학생 10% 할인");
    assert_eq!(value_proposals(&pool, &f).await, 0);

    // A moved page is not adopted in shadow either.
    route(&site, "/benefit", 301, "", Some("/students"));
    route(&site, "/students", 200, &nike_page("15", "30", "12"), None);
    pass(&pool, &f, VerifyMode::Shadow).await;
    let moved: String = sqlx::query_scalar("SELECT url FROM benefit_source_candidates WHERE id=$1")
        .bind(f.candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(moved, url);
    cleanup(&pool, &f).await;
}

// ── round 8: hide_only, expired adoption ──────────────────────────────────

async fn fact_statuses(pool: &PgPool, f: &Fixture) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM corporate_offer_facts WHERE program_id=$1 ORDER BY id")
        .bind(f.program_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn last_detail(pool: &PgPool, f: &Fixture) -> Value {
    sqlx::query_scalar(
        "SELECT details FROM corporate_evidence_checks WHERE candidate_id=$1 ORDER BY checked_at DESC, id LIMIT 1",
    )
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// hide_only: a hide for an allowlisted reason applies; a reason that is
/// not allowlisted is only counted; nothing ever becomes visible.
#[tokio::test]
async fn hide_only_hides_for_allowlisted_reasons_and_never_shows() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    assert!(
        std::env::var("CORPORATE_AUTO_HIDE_REASONS").is_err(),
        "the default allowlist"
    );
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;

    // Held at the start; a confirming page in hide_only restores nothing.
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), false));
    assert!(!state(&pool, &f).await.0, "hide_only never shows");

    // Live shows it.
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);

    // A reason not on the allowlist (the quote changed) is only counted.
    site.page.lock().unwrap().1 = nike_page("15", "30", "12");
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("quote_missing".into(), false)
    );
    assert!(state(&pool, &f).await.0, "not allowlisted: still shown");
    assert!(fact_statuses(&pool, &f)
        .await
        .iter()
        .all(|s| s == "published"));

    // One signal alone (a passed own period) is a shadow metric now.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p>",
    );
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(last_outcome(&pool, &f).await, ("ambiguous".into(), false));
    assert!(state(&pool, &f).await.0, "one signal: still shown");

    // Two signals (the offer's own period passed and the offer said to
    // have ended) are a shadow metric by default too ...
    site.page.lock().unwrap().1 = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><p>이벤트 기간: 2026.09.01 ~ 2026.10.13</p><p>본 이벤트는 종료되었습니다.</p>",
    );
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(last_outcome(&pool, &f).await, ("ambiguous".into(), false));
    assert!(state(&pool, &f).await.0, "default allowlist: still shown");
    // ... and hide for real once an operator lists the reason.
    std::env::set_var(
        "CORPORATE_AUTO_HIDE_REASONS",
        "section_ended_confirmed,source_gone",
    );
    pass(&pool, &f, VerifyMode::HideOnly).await;
    std::env::remove_var("CORPORATE_AUTO_HIDE_REASONS");
    assert_eq!(last_outcome(&pool, &f).await, ("ambiguous".into(), true));
    assert!(!state(&pool, &f).await.0, "allowlisted: hidden");
    let detail = last_detail(&pool, &f).await;
    assert!(
        detail
            .to_string()
            .contains(super::resilience::SECTION_ENDED_CONFIRMED),
        "{detail}"
    );

    // The page is fine again: hide_only does not bring it back.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12");
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert!(!state(&pool, &f).await.0, "hide_only never restores");
    cleanup(&pool, &f).await;
}

/// hide_only: a source gone for good is held (allowlisted `source_gone`),
/// not retired; a first 404 is not enough.
#[tokio::test]
async fn hide_only_holds_a_source_gone_for_good() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    *site.page.lock().unwrap() = (404, "not found".into());
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(last_outcome(&pool, &f).await, ("gone".into(), false));
    assert!(state(&pool, &f).await.0, "one 404 is not gone for good");
    sqlx::query("UPDATE corporate_evidence_targets SET first_gone_at=now()-interval '25 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::HideOnly).await;
    assert_eq!(last_outcome(&pool, &f).await, ("retired_gone".into(), true));
    let (active, status): (bool, String) =
        sqlx::query_as("SELECT is_active, program_status FROM programs WHERE id=$1")
            .bind(f.program_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!active);
    assert_ne!(status, "closed", "held, not retired");
    cleanup(&pool, &f).await;
}

/// An expired adoption decides nothing in live: a hidden fact is never
/// published on its account.
#[tokio::test]
async fn an_expired_adoption_never_publishes_in_live() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    sqlx::query(
        "UPDATE corporate_offer_facts SET status='hidden', adopt_before=now()-interval '1 day',
                window_signature=NULL, shadow_window_signature=NULL WHERE program_id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "adoption_expired");
    assert!(fact_statuses(&pool, &f).await.iter().all(|s| s == "hidden"));
    assert!(!state(&pool, &f).await.0);
    cleanup(&pool, &f).await;
}

/// The first sight is stored with the fact and kept while the page reads
/// the same: yearless dates are read against it on later runs.
#[tokio::test]
async fn the_first_sight_is_kept_while_the_page_reads_the_same() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let html = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><p>이벤트 기간 ~ 12월 31일까지</p>",
    );
    let site = site_with(html);
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    let seen = |pool: PgPool, id: Uuid| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT DISTINCT anchors->'fingerprint'->>'seen' FROM corporate_offer_facts WHERE program_id=$1",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    assert_eq!(seen(pool.clone(), f.program_id).await.len(), 1);
    // An earlier first sight on the same page is kept by the next
    // confirmation.
    sqlx::query(
        "UPDATE corporate_offer_facts SET anchors = jsonb_set(anchors, '{fingerprint,seen}', '\"2026-10-01\"') WHERE program_id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    assert_eq!(
        seen(pool.clone(), f.program_id).await,
        vec![Some("2026-10-01".to_string())]
    );
    // Read against a first sight in the spring, 12월 31일 is still ahead but
    // a 3월 31일 end would have passed: the stored day decides.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><p>이벤트 기간 ~ 3월 31일까지</p>",
    );
    sqlx::query(
        "UPDATE corporate_offer_facts SET anchors = jsonb_set(anchors, '{fingerprint,seen}', '\"2026-03-01\"') WHERE program_id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(!state(&pool, &f).await.0);
    let detail = last_detail(&pool, &f).await.to_string();
    assert!(
        detail.contains(super::resilience::SECTION_DATE_PAST),
        "{detail}"
    );
    cleanup(&pool, &f).await;
}

// The reviewer's round-8 DB module, kept verbatim as a gate. Its first
// case hides with one passed own period, which round 9 no longer trusts by
// default; the invariant it tests (the hash monitor never releases an
// evidence hold) holds for any allowlist. `r8_db_gate` runs the module in a
// child process with `CORPORATE_AUTO_HIDE_REASONS=section_date_past,
// source_gone`; in the direct run its database cases skip (`db()` below).
#[path = "resilience_r8_db_host.rs"]
mod r8_db_host;

/// Runs the round-8 DB module with the allowlist its cases assume.
#[tokio::test]
async fn r8_db_gate() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            &format!("{module}::r8_db_host::adversarial_r8_db::"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("TEST_DATABASE_URL", url)
        .env("R8_DB_GATE", "1")
        .env(
            "CORPORATE_AUTO_HIDE_REASONS",
            "section_date_past,source_gone",
        )
        .output()
        .expect("child test run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("3 passed"), "{stdout}");
    assert!(
        stdout.contains("R8DB hide_only outcome=ambiguous applied=true active=false"),
        "{stdout}"
    );
}

async fn first_gone(pool: &PgPool, f: &Fixture) -> Option<chrono::DateTime<Utc>> {
    sqlx::query_scalar("SELECT first_gone_at FROM corporate_evidence_targets WHERE candidate_id=$1")
        .bind(f.candidate_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A 404 counts toward retirement only once a re-fetch at least an hour
/// later reads it too.
#[tokio::test]
async fn a_404_counts_only_after_a_confirming_refetch_an_hour_later() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    // A blip: 404, then the page is back.
    *site.page.lock().unwrap() = (404, "not found".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "gone");
    assert_eq!(
        first_gone(&pool, &f).await,
        None,
        "the first 404 only schedules a re-fetch"
    );
    let next: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM next_check_at - now())/60)::float8 FROM corporate_evidence_targets WHERE candidate_id=$1")
        .bind(f.candidate_id).fetch_one(&pool).await.unwrap();
    assert!(
        (55.0..=65.0).contains(&next),
        "re-fetched in an hour: {next}"
    );
    *site.page.lock().unwrap() = (200, nike_page("10", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(first_gone(&pool, &f).await, None);
    // Two 404s within the hour: still not counted.
    *site.page.lock().unwrap() = (404, "not found".into());
    pass(&pool, &f, VerifyMode::Live).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        first_gone(&pool, &f).await,
        None,
        "two 404s within the hour"
    );
    // A re-fetch an hour later that still reads 404 starts the clock.
    sqlx::query("UPDATE corporate_evidence_targets SET last_checked_at = now() - interval '2 hours' WHERE candidate_id=$1")
        .bind(f.candidate_id).execute(&pool).await.unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(
        first_gone(&pool, &f).await.is_some(),
        "confirmed an hour later"
    );
    cleanup(&pool, &f).await;
}

// ── migration 20261018000015: Nike audience, Spotify duplicate ────────────

const FIX_15: &str = include_str!(
    "../../../../infra/migrations/20261018000015_corporate_nike_audience_spotify_duplicate.sql"
);

async fn insert_programme(pool: &PgPool, id: Uuid, source_id: &str, title: &str, active: bool) {
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions,normalized_payload)
                 VALUES($1,'corporate_benefit','corporate_official',$2,$3,'요약','픽스처','https://www.spotify.com/kr-ko/student/','always_open','corporate','subscription',$4,'current',ARRAY[]::text[],'{}'::jsonb)")
        .bind(id).bind(source_id).bind(title).bind(active).execute(pool).await.unwrap();
}

async fn bookmark(pool: &PgPool, program: Uuid) -> Uuid {
    let user: Uuid = sqlx::query_scalar("INSERT INTO users(auth_provider,auth_provider_id,role) VALUES('test',$1,'user') RETURNING id")
        .bind(Uuid::new_v4().to_string()).fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(program)
        .execute(pool)
        .await
        .unwrap();
    user
}

/// Runs the migration twice (it must be idempotent) and reads the two
/// Spotify rows: (active, retirement reason, duplicate_of).
async fn spotify_after(pool: &PgPool) -> Vec<(Uuid, bool, Option<String>, Option<String>)> {
    for _ in 0..2 {
        sqlx::raw_sql(FIX_15).execute(pool).await.unwrap();
    }
    sqlx::query_as("SELECT id, is_active, retirement_reason, normalized_payload->>'duplicate_of' FROM programs
                    WHERE id IN ('0daf688b-691c-44d6-9e5a-f3ea9f481a34','7af27e95-3a01-41f6-8639-6a9585cec9b9') ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

async fn new_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO users(auth_provider,auth_provider_id,role) VALUES('test',$1,'user') RETURNING id")
        .bind(Uuid::new_v4().to_string())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Test data only: removes the two Spotify fixture rows and every row
/// that points at them.
async fn clear_spotify(pool: &PgPool) {
    sqlx::raw_sql(
        "DO $clear$ DECLARE ref RECORD; BEGIN
           FOR ref IN SELECT DISTINCT kcu.table_name::text AS t, kcu.column_name::text AS c
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name AND kcu.constraint_schema = tc.constraint_schema
             JOIN information_schema.constraint_column_usage ccu ON ccu.constraint_name = tc.constraint_name AND ccu.constraint_schema = tc.constraint_schema
             WHERE tc.constraint_type = 'FOREIGN KEY' AND ccu.table_name = 'programs' AND tc.table_name <> 'programs'
           LOOP
             EXECUTE format('DELETE FROM public.%I WHERE %I IN (''0daf688b-691c-44d6-9e5a-f3ea9f481a34'', ''7af27e95-3a01-41f6-8639-6a9585cec9b9'')', ref.t, ref.c);
           END LOOP;
           DELETE FROM programs WHERE id IN ('0daf688b-691c-44d6-9e5a-f3ea9f481a34', '7af27e95-3a01-41f6-8639-6a9585cec9b9');
         END $clear$;",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Every reviewer case for migration 15's Spotify step: references in
/// any user table decide, the retire_as_duplicate guard keeps a manual
/// retirement, a hold is no retirement, a new user table counts without
/// editing the migration.
#[tokio::test]
async fn migration_15_keeps_the_referenced_spotify_programme() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let a: Uuid = "0daf688b-691c-44d6-9e5a-f3ea9f481a34".parse().unwrap();
    let b: Uuid = "7af27e95-3a01-41f6-8639-6a9585cec9b9".parse().unwrap();
    let setup = |pool: PgPool, a_active: bool, b_active: bool| async move {
        clear_spotify(&pool).await;
        insert_programme(
            &pool,
            a,
            "fixture:spotify-a",
            "Spotify Premium 학생 요금제",
            a_active,
        )
        .await;
        insert_programme(
            &pool,
            b,
            "fixture:spotify-b",
            "Spotify Premium 학생 요금제",
            b_active,
        )
        .await;
    };
    let kept = |id: Uuid| (id, true, None, None);
    let duplicate_of = |id: Uuid, of: Uuid| {
        (
            id,
            false,
            Some("duplicate".to_string()),
            Some(of.to_string()),
        )
    };

    // C1: no references, both shown: the first is kept.
    setup(pool.clone(), true, true).await;
    assert_eq!(
        spotify_after(&pool).await,
        vec![kept(a), duplicate_of(b, a)],
        "C1"
    );

    // C2: b bookmarked: b kept.
    setup(pool.clone(), true, true).await;
    bookmark(&pool, b).await;
    assert_eq!(
        spotify_after(&pool).await,
        vec![duplicate_of(a, b), kept(b)],
        "C2"
    );

    // C3: b bookmarked, a in a user's state: both kept.
    setup(pool.clone(), true, true).await;
    bookmark(&pool, b).await;
    let user = new_user(&pool).await;
    sqlx::query(
        "INSERT INTO user_program_states(user_id,program_id,state) VALUES($1,$2,'interested')",
    )
    .bind(user)
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(spotify_after(&pool).await, vec![kept(a), kept(b)], "C3");

    // C4: a only in an alert subscription, b bookmarked: both kept (the
    // alert is a reference too).
    setup(pool.clone(), true, true).await;
    let user = new_user(&pool).await;
    sqlx::query("INSERT INTO alert_subscriptions(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    bookmark(&pool, b).await;
    assert_eq!(spotify_after(&pool).await, vec![kept(a), kept(b)], "C4");

    // C5: b retired by hand: never overwritten.
    setup(pool.clone(), true, false).await;
    sqlx::query("UPDATE programs SET retirement_reason='manual', retired_at=now()-interval '3 days' WHERE id=$1")
        .bind(b).execute(&pool).await.unwrap();
    assert_eq!(
        spotify_after(&pool).await,
        vec![kept(a), (b, false, Some("manual".into()), None)],
        "C5"
    );

    // C6: a held (not retired), b shown, no references: b kept, a stays a
    // hold, not a duplicate retirement.
    setup(pool.clone(), false, true).await;
    sqlx::query("UPDATE programs SET program_status='review_required' WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        spotify_after(&pool).await,
        vec![(a, false, None, None), kept(b)],
        "C6"
    );

    // C7: only the hidden copy referenced, the shown one not: both kept.
    setup(pool.clone(), true, false).await;
    bookmark(&pool, b).await;
    let rows = spotify_after(&pool).await;
    assert!(
        rows.iter().all(|(_, _, reason, _)| reason.is_none()),
        "C7 {rows:?}"
    );

    // C8: a user table added later, not named in the migration: counted.
    sqlx::query("CREATE TABLE m15_probe_refs (user_id UUID REFERENCES users(id), program_id UUID REFERENCES programs(id) ON DELETE CASCADE)")
        .execute(&pool).await.unwrap();
    setup(pool.clone(), true, true).await;
    let user = new_user(&pool).await;
    sqlx::query("INSERT INTO m15_probe_refs VALUES($1,$2)")
        .bind(user)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    let rows = spotify_after(&pool).await;
    sqlx::query("DROP TABLE m15_probe_refs")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![duplicate_of(a, b), kept(b)], "C8");

    // C9: the migration role (a member of the owner, not a superuser) runs
    // it; a table with a program_id column owned by another role is hidden
    // from it: both kept. The same role without that table retires as C1.
    let as_role = |pool: PgPool| async move {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::raw_sql("SET ROLE m15_probe")
            .execute(&mut *conn)
            .await
            .unwrap();
        for _ in 0..2 {
            sqlx::raw_sql(FIX_15).execute(&mut *conn).await.unwrap();
        }
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query_as::<_, (Uuid, bool, Option<String>, Option<String>)>(
            "SELECT id, is_active, retirement_reason, normalized_payload->>'duplicate_of' FROM programs
             WHERE id IN ('0daf688b-691c-44d6-9e5a-f3ea9f481a34','7af27e95-3a01-41f6-8639-6a9585cec9b9') ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    sqlx::raw_sql(
        "DROP ROLE IF EXISTS m15_probe; DROP ROLE IF EXISTS m15_other;
         CREATE ROLE m15_probe BYPASSRLS; CREATE ROLE m15_other;
         DO $g$ BEGIN EXECUTE format('GRANT %I TO m15_probe', current_user); END $g$;
         GRANT REFERENCES ON programs TO m15_other;",
    )
    .execute(&pool)
    .await
    .unwrap();
    setup(pool.clone(), true, true).await;
    assert_eq!(
        as_role(pool.clone()).await,
        vec![kept(a), duplicate_of(b, a)],
        "C9 control"
    );
    setup(pool.clone(), true, true).await;
    sqlx::raw_sql(
        "CREATE TABLE m15_probe_hidden (user_id UUID, program_id UUID REFERENCES programs(id) ON DELETE CASCADE);
         ALTER TABLE m15_probe_hidden OWNER TO m15_other;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let rows = as_role(pool.clone()).await;
    sqlx::raw_sql(
        "DROP TABLE m15_probe_hidden; REVOKE REFERENCES ON programs FROM m15_other;
         DROP ROLE m15_probe; DROP ROLE m15_other;",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![kept(a), kept(b)], "C9");
    clear_spotify(&pool).await;
}

/// Nike: 대학생 copy, and a rule the rule engine reads through its
/// canonical school type: 대학교, 전문대학 and university students pass,
/// high-school profiles fail, whatever spelling; an existing rule is never
/// overwritten.
#[tokio::test]
async fn migration_15_fixes_nike_for_every_school_type_spelling() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    for _ in 0..2 {
        sqlx::raw_sql(FIX_15).execute(&pool).await.unwrap();
    }
    let nike: Uuid = sqlx::query_scalar("SELECT id FROM programs WHERE source_type='corporate_official' AND source_id='verified:20260923:nike'")
        .fetch_one(&pool).await.unwrap();
    let (title, summary, conditions, active): (String, String, Option<String>, bool) = sqlx::query_as(
        "SELECT title, summary, normalized_payload->>'conditions', is_active FROM programs WHERE id=$1",
    )
    .bind(nike).fetch_one(&pool).await.unwrap();
    assert_eq!(title, "나이키 대학생 10% 할인");
    assert!(summary.starts_with("대학생 자격 인증 후"), "{summary}");
    assert_eq!(
        conditions.as_deref(),
        Some("대학생 자격 인증, 대상 상품 및 제외 조건 확인")
    );
    assert!(active, "nothing retired");
    let rules: Vec<(serde_json::Value, Option<String>)> = sqlx::query_as(
        "SELECT hard_filter_json, created_by FROM eligibility_rules WHERE program_id=$1",
    )
    .bind(nike)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rules.len(), 1, "one rule, inserted once: {rules:?}");
    let rule = mazimi_core::rule_engine::parse_rule_json(&rules[0].0).unwrap();
    use mazimi_core::rule_engine::RuleVerdict;
    let verdict = |school_type: &str, enrollment: &str| {
        let profile: mazimi_core::models::ProfileInput = serde_json::from_value(json!({
            "birth_year": 2005, "region_code": "busan",
            "school_type": school_type, "enrollment_status": enrollment, "school_year": 2
        }))
        .unwrap();
        mazimi_core::rule_engine::evaluate(
            &profile,
            &rule,
            &mazimi_core::rule_engine::AgeBasis::today(),
        )
        .verdict
    };
    for school_type in [
        "대학교",
        "university",
        "University",
        "4년제",
        "전문대학",
        "college",
    ] {
        assert_eq!(
            verdict(school_type, "enrolled"),
            RuleVerdict::Pass,
            "{school_type}"
        );
    }
    for school_type in ["고등학교", "high_school", "고교"] {
        assert_eq!(
            verdict(school_type, "enrolled"),
            RuleVerdict::Fail,
            "{school_type}"
        );
    }
    // A 대학원 student is read as 졸업 or 석·박사: asked, never hidden.
    assert_eq!(verdict("대학원", "enrolled"), RuleVerdict::Unknown);
    // An existing (human) rule is left as it is.
    sqlx::query("UPDATE eligibility_rules SET created_by='human' WHERE program_id=$1")
        .bind(nike)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(FIX_15).execute(&pool).await.unwrap();
    let after: Vec<Option<String>> =
        sqlx::query_scalar("SELECT created_by FROM eligibility_rules WHERE program_id=$1")
            .bind(nike)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(after, vec![Some("human".to_string())]);
    sqlx::query(
        "UPDATE eligibility_rules SET created_by='migration:20261018000015' WHERE program_id=$1",
    )
    .bind(nike)
    .execute(&pool)
    .await
    .unwrap();
}

// ── review deadlines: evidence keeps them, the overdue sweep never starves ──

fn verify_env(mode: &str) {
    std::env::set_var("CORPORATE_AUTO_VERIFY_ENABLED", "1");
    std::env::set_var("CORPORATE_AUTO_VERIFY_MODE", mode);
    std::env::set_var(
        "CORPORATE_BOT_CONTACT_URL",
        "https://mazimi.example/crawler-policy",
    );
    std::env::remove_var("CORPORATE_AUTO_HIDE_REASONS");
}

fn clear_verify_env() {
    for key in [
        "CORPORATE_AUTO_VERIFY_ENABLED",
        "CORPORATE_AUTO_VERIFY_MODE",
        "CORPORATE_BOT_CONTACT_URL",
        "CORPORATE_AUTO_HIDE_REASONS",
    ] {
        std::env::remove_var(key);
    }
}

async fn review_due(pool: &PgPool, f: &Fixture) -> chrono::DateTime<Utc> {
    sqlx::query_scalar("SELECT review_due_at FROM program_source_monitors WHERE program_id=$1")
        .bind(f.program_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One simulated day: every clock on the source moves back a day.
async fn a_day_passes(pool: &PgPool, f: &Fixture) {
    sqlx::query(
        "UPDATE program_source_monitors
         SET review_due_at = review_due_at - interval '1 day',
             verified_at = verified_at - interval '1 day'
         WHERE program_id = $1",
    )
    .bind(f.program_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE corporate_evidence_targets
         SET last_checked_at = last_checked_at - interval '1 day',
             next_check_at = next_check_at - interval '1 day'
         WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(pool)
    .await
    .unwrap();
}

/// 30 simulated days on a page that stays fine, in shadow and in
/// hide_only: the evidence pass moves the review deadline every day, the
/// overdue sweep and the upcoming warning never hide the offer. Without
/// the move, the sweep held it on day 17 (14-day deadline, 72 h grace).
async fn stable_page_for_30_days(mode: VerifyMode, env_mode: &str) {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0, "published by a live confirmation");
    verify_env(env_mode);
    for day in 1..=30 {
        a_day_passes(&pool, &f).await;
        pass(&pool, &f, mode).await;
        assert!(
            review_due(&pool, &f).await > Utc::now() + chrono::Duration::days(13),
            "day {day}: the deadline moved"
        );
        crate::source_monitoring::warn_upcoming_manual_reviews(&pool)
            .await
            .unwrap();
        let swept = crate::source_monitoring::hold_overdue_sources(
            &pool,
            chrono::Duration::hours(72),
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(swept.programs_held, 0, "day {day}");
        assert!(state(&pool, &f).await.0, "day {day}: still shown");
    }
    clear_verify_env();
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn hide_only_keeps_a_stable_offer_shown_for_30_days() {
    stable_page_for_30_days(VerifyMode::HideOnly, "hide_only").await;
}

#[tokio::test]
async fn shadow_keeps_a_stable_offer_shown_for_30_days() {
    stable_page_for_30_days(VerifyMode::Shadow, "shadow").await;
}

/// The overdue sweep holds a source past its deadline in every mode,
/// evidence_auto included (fail closed); in shadow an allowlisted reason
/// moves no deadline, and an unreadable source past it is held.
#[tokio::test]
async fn the_overdue_sweep_holds_unconfirmed_sources_in_every_mode() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let backdate = |pool: PgPool, id: Uuid| async move {
        sqlx::query(
            "UPDATE program_source_monitors SET review_due_at = now() - interval '30 days' WHERE program_id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    };
    let sweep = |pool: PgPool| async move {
        crate::source_monitoring::hold_overdue_sources(&pool, chrono::Duration::hours(72), 1_000)
            .await
            .unwrap()
            .programs_held
    };
    for mode in ["hide_only", "live"] {
        let site = site_with(nike_page("10", "30", "12"));
        let url = serve(site.clone()).await;
        let f = fixture(&pool, &url).await;
        pass(&pool, &f, VerifyMode::Live).await;
        assert!(state(&pool, &f).await.0);
        verify_env(mode);
        backdate(pool.clone(), f.program_id).await;
        assert_eq!(sweep(pool.clone()).await, 1, "{mode}: fail closed");
        assert!(!state(&pool, &f).await.0, "{mode}");
        clear_verify_env();
        cleanup(&pool, &f).await;
    }
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    // Shadow, an allowlisted reason on the page: counted, not applied, and
    // the deadline is not moved on its account.
    verify_env("shadow");
    std::env::set_var(
        "CORPORATE_AUTO_HIDE_REASONS",
        "section_ended_confirmed,source_gone",
    );
    site.page.lock().unwrap().1 = nike_page("10", "30", "12").replace(
        "<p>대상 상품 최대 12개월</p>",
        "<p>대상 상품 최대 12개월</p><dl><dt>이벤트 기간</dt><dd>2026.09.01 ~ 2026.10.13</dd></dl><p>본 이벤트는 종료되었습니다.</p>",
    );
    backdate(pool.clone(), f.program_id).await;
    pass(&pool, &f, VerifyMode::Shadow).await;
    assert!(state(&pool, &f).await.0, "shadow applies nothing");
    assert!(review_due(&pool, &f).await < Utc::now());
    // The source unreadable past its deadline: held.
    site.page.lock().unwrap().0 = 500;
    pass(&pool, &f, VerifyMode::Shadow).await;
    assert_eq!(sweep(pool.clone()).await, 1, "shadow: fail closed");
    assert!(!state(&pool, &f).await.0);
    clear_verify_env();
    cleanup(&pool, &f).await;
}

/// The evidence selector takes a target whose monitor's deadline is due
/// even when its own schedule is a day away, at most hourly; health counts
/// only what the selector should have checked and did not.
#[tokio::test]
async fn a_due_deadline_is_checked_and_health_agrees() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    verify_env("shadow");
    let missed = |pool: PgPool| async move {
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM program_source_monitors monitor
             LEFT JOIN corporate_evidence_targets target ON target.candidate_id = monitor.candidate_id
             WHERE monitor.verification_mode = 'evidence_auto' AND monitor.verification_state <> 'retired'
               AND (target.candidate_id IS NULL
                    OR target.next_check_at <= now() - interval '2 hours'
                    OR ({} AND (target.last_checked_at IS NULL
                                OR target.last_checked_at <= now() - interval '3 hours')))",
            super::DEADLINE_DUE_SQL
        ))
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let run = |pool: PgPool, id: Uuid| async move {
        let http = LoopbackHttp::new(UA);
        run_with(
            &pool,
            VerifyMode::Shadow,
            &http,
            Some(vec![id]),
            Some(std::time::Duration::from_millis(20)),
        )
        .await
        .unwrap()
        .targets
    };
    // Schedule a day away, deadline passed, last read four hours ago: the
    // health count sees a missed check and the selector takes it.
    sqlx::query(
        "UPDATE corporate_evidence_targets SET next_check_at = now() + interval '1 day',
                last_checked_at = now() - interval '4 hours' WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE program_source_monitors SET review_due_at = now() - interval '1 hour' WHERE program_id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(missed(pool.clone()).await, 1);
    assert_eq!(run(pool.clone(), f.candidate_id).await, 1);
    assert!(review_due(&pool, &f).await > Utc::now() + chrono::Duration::days(13));
    assert_eq!(missed(pool.clone()).await, 0, "checked: no alarm");
    // Read within the hour: not taken again.
    sqlx::query("UPDATE program_source_monitors SET review_due_at = now() - interval '1 hour' WHERE program_id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(run(pool.clone(), f.candidate_id).await, 0);
    assert_eq!(missed(pool.clone()).await, 0, "checked within the hour");
    clear_verify_env();
    cleanup(&pool, &f).await;
}

async fn db_pool() -> Option<PgPool> {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping database-backed evidence test");
        return None;
    };
    Some(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .unwrap(),
    )
}

/// The fixture is the production shape since 2026-09-23: an evidence_auto
/// monitor past its deadline whose offer the overdue sweep already hid. It
/// is a digest count, not an alarm; the same monitor on an offer still
/// shown (the sweep failed to hide it) is the alarm.
#[tokio::test]
async fn past_deadline_alarm_counts_only_offers_still_shown() {
    let Some(pool) = db_pool().await else { return };
    let _guard = DB_LOCK.lock().await;
    let counts = |pool: PgPool| async move {
        let metrics = super::super::health::collect(&pool).await.unwrap();
        (
            metrics["monitors"]["past_deadline_evidence_auto"]
                .as_i64()
                .unwrap(),
            metrics["monitors"]["past_deadline_visible_evidence_auto"]
                .as_i64()
                .unwrap(),
        )
    };
    let before = counts(pool.clone()).await;
    let f = fixture(&pool, "http://127.0.0.1:9/benefit").await;
    let held = counts(pool.clone()).await;
    assert_eq!(
        (held.0 - before.0, held.1 - before.1),
        (1, 0),
        "a held past-deadline offer is counted, not alarmed"
    );
    sqlx::query(
        "UPDATE programs SET is_active = true, program_status = 'always_open', source_health = 'current'
         WHERE id = $1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    let leaked = counts(pool.clone()).await;
    assert_eq!((leaked.0 - before.0, leaked.1 - before.1), (1, 1));
    // `current` and past its deadline but inside the sweep's grace (72 h by
    // default): shown by design, counted but not alarmed.
    sqlx::query(
        "UPDATE program_source_monitors SET verification_state = 'current',
             review_due_at = now() - interval '1 hour' WHERE program_id = $1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    let in_grace = counts(pool.clone()).await;
    assert_eq!((in_grace.0 - before.0, in_grace.1 - before.1), (1, 0));
    let alerts = super::super::health::evaluate(
        &json!({"monitors": {"past_deadline_visible_evidence_auto": leaked.1}}),
        None,
        false,
    );
    assert_eq!(alerts.past_deadline_evidence_auto, Some(leaked.1));
    cleanup(&pool, &f).await;
}

/// 공차 (c1499b35) was retired by a data-fix migration that set its monitor
/// `retired` but left `source_review:<candidate>` open. Source maintenance
/// resolves the events of a candidate whose monitors are all retired, and
/// leaves those of a candidate with a live monitor alone.
#[tokio::test]
async fn source_events_of_a_fully_retired_candidate_resolve_in_maintenance() {
    let Some(pool) = db_pool().await else { return };
    let _guard = DB_LOCK.lock().await;
    let f = fixture(&pool, "http://127.0.0.1:9/benefit").await;
    let upcoming = format!("source_review_upcoming:{}", f.candidate_id);
    operations::record_event(
        &pool,
        &operations::EventCommand {
            fingerprint: &upcoming,
            severity: operations::Severity::Warning,
            component: "source_monitoring",
            event_type: "source_review_upcoming",
            subject_type: Some("benefit_source_candidate"),
            subject_id: Some(&f.candidate_id.to_string()),
            message: "upcoming",
            details: json!({}),
        },
    )
    .await
    .unwrap();
    let event = |fingerprint: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT status, resolution_note FROM operational_events WHERE fingerprint = $1",
            )
            .bind(fingerprint)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let review = format!("source_review:{}", f.candidate_id);

    // A live (review_due) monitor: the review is real and stays open.
    crate::source_monitoring::resolve_retired_source_events(&pool)
        .await
        .unwrap();
    assert_eq!(event(review.clone()).await.0, "open");

    sqlx::query(
        "UPDATE program_source_monitors SET verification_state = 'retired' WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let (grace, limit) = (chrono::Duration::hours(72), 100);
    let summary = crate::source_monitoring::run_source_maintenance(&pool, grace, limit)
        .await
        .unwrap();
    assert!(summary.retired_source_events_resolved >= 1, "{summary:?}");
    let (status, note) = event(review).await;
    assert_eq!(status, "resolved");
    assert!(note.unwrap_or_default().contains("is retired"));
    // The upcoming warning already closed through its own condition (no
    // current monitor); either way it is resolved.
    assert_eq!(event(upcoming).await.0, "resolved");
    cleanup(&pool, &f).await;
}

/// Removes the weekly-audit flag even when the test fails.
struct AuditFlag;

impl AuditFlag {
    fn on() -> Self {
        std::env::set_var(super::super::weekly_audit::ENABLED_FLAG, "true");
        Self
    }
}

impl Drop for AuditFlag {
    fn drop(&mut self) {
        std::env::remove_var(super::super::weekly_audit::ENABLED_FLAG);
    }
}

/// Everything one pass decided for a fixture, ids, ports and times left out.
async fn decided(pool: &PgPool, f: &Fixture) -> Value {
    let checks: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('mode', mode, 'outcome', outcome, 'applied', applied,
                                   'http_status', http_status, 'details', details)
         FROM corporate_evidence_checks WHERE candidate_id = $1 ORDER BY checked_at, id",
    )
    .bind(f.candidate_id)
    .fetch_all(pool)
    .await
    .unwrap();
    let facts: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('status', status, 'last_outcome', last_outcome,
                                   'window_signature', window_signature)
         FROM corporate_offer_facts WHERE program_id = $1 ORDER BY quote",
    )
    .bind(f.program_id)
    .fetch_all(pool)
    .await
    .unwrap();
    let (active, status, monitor, holds, note, event) = state(pool, f).await;
    let text = json!({
        "checks": checks, "facts": facts,
        "state": [active, status, monitor, holds, note, event],
    })
    .to_string();
    let ids = regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32}|127\.0\.0\.1:\d+").unwrap();
    serde_json::from_str(&ids.replace_all(&text, "<id>")).unwrap()
}

#[tokio::test]
async fn the_weekly_audit_hook_never_changes_an_evidence_pass() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let run = |f: &Fixture| {
        let pool = pool.clone();
        let candidate = f.candidate_id;
        async move {
            let http = LoopbackHttp::new(UA);
            super::run_with_publish(
                &pool,
                VerifyMode::Live,
                false,
                false,
                &http,
                Some(vec![candidate]),
                Some(std::time::Duration::from_millis(20)),
            )
            .await
            .unwrap()
        }
    };
    let verdicts = |f: &Fixture| {
        let pool = pool.clone();
        let program = f.program_id;
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM corporate_weekly_audit_verdicts WHERE program_id = $1",
            )
            .bind(program)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let off_site = site_with(nike_page("10", "30", "12"));
    let on_site = site_with(nike_page("10", "30", "12"));
    // Each fixture writes the newest census run, so each pass runs right
    // after its own fixture.
    let off = fixture(&pool, &serve(off_site.clone()).await).await;
    let off_summary = run(&off).await;
    let on = fixture(&pool, &serve(on_site.clone()).await).await;
    let on_summary = {
        let _flag = AuditFlag::on();
        run(&on).await
    };
    // The hook ran (its own task): a verdict appears for the flagged pass.
    let mut waited = 0;
    while verdicts(&on).await == 0 && waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited += 1;
    }
    assert_eq!(verdicts(&on).await, 1, "the hook branch was exercised");
    assert_eq!(verdicts(&off).await, 0);

    assert_eq!(
        (
            off_summary.targets,
            &off_summary.decisions,
            off_summary.applied,
            off_summary.requests
        ),
        (
            on_summary.targets,
            &on_summary.decisions,
            on_summary.applied,
            on_summary.requests
        )
    );
    assert!(off_summary.requests > 0);
    assert_eq!(
        off_site.user_agents.lock().unwrap().len(),
        on_site.user_agents.lock().unwrap().len(),
        "the hook sends no request of its own"
    );
    assert_eq!(decided(&pool, &off).await, decided(&pool, &on).await);
    cleanup(&pool, &off).await;
    cleanup(&pool, &on).await;
}

/// The daily schedule keeps the hour the run started: a target checked
/// late in an hourly run is due again at the same run the next day, not an
/// hour later each day (the next check was `now() + 1 day` per target, so
/// the daily check slid by the run's length every day).
#[tokio::test]
async fn the_daily_schedule_keeps_the_hour_the_run_started() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let next = |pool: PgPool, candidate: Uuid| async move {
        sqlx::query_as::<_, (f64, f64)>(
            "SELECT extract(epoch FROM next_check_at - date_trunc('hour', next_check_at))::float8,
                    (extract(epoch FROM next_check_at - now()) / 3600)::float8
             FROM corporate_evidence_targets WHERE candidate_id = $1",
        )
        .bind(candidate)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    // A page read: due again a day after the hour the run started.
    pass(&pool, &f, VerifyMode::Live).await;
    let (past_hour, hours) = next(pool.clone(), f.candidate_id).await;
    assert_eq!(past_hour, 0.0, "on the hour of the run start");
    assert!((23.0..=24.0).contains(&hours), "{hours}");
    // A robots block inside grace: looked at daily from the same hour,
    // plus the source's whole-hour jitter (at most a quarter).
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: MazimiBenefitBot\nDisallow: /benefit\n".into(),
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "robots_disallowed");
    let (past_hour, hours) = next(pool.clone(), f.candidate_id).await;
    assert_eq!(past_hour, 0.0);
    assert!((23.0..=30.0).contains(&hours), "{hours}");
    cleanup(&pool, &f).await;
}

#[test]
fn the_next_check_is_counted_from_the_hour_the_run_started() {
    let started = Utc.with_ymd_and_hms(2026, 10, 20, 8, 47, 13).unwrap();
    assert_eq!(
        next_check_at(started, chrono::Duration::days(1)),
        Utc.with_ymd_and_hms(2026, 10, 21, 8, 0, 0).unwrap()
    );
    assert_eq!(
        next_check_at(started, chrono::Duration::days(7)),
        Utc.with_ymd_and_hms(2026, 10, 27, 8, 0, 0).unwrap()
    );
}

// ── the current-fact rule: a page changed elsewhere ───────────────────────

/// The Nike offer in a section of its own, a navigation, and another
/// offer's section (so the page carries two offers and the Nike offer's
/// scope is its own section). `nav` names one of four orders of the same three links
/// (`러닝화 컬렉션` first, `패딩 컬렉션` first, `플리스 컬렉션` first,
/// `트레이닝 컬렉션` for a fourth); any other text replaces the first link
/// (a rename).
fn sectioned_nike(nav: &str) -> String {
    let link = |label: &str| {
        let href = match label {
            "러닝화" => "/run",
            "패딩" => "/pad",
            "플리스" => "/fleece",
            _ => "/new",
        };
        format!("<li><a href=\"{href}\">{label}</a></li>")
    };
    let order: Vec<&str> = match nav {
        "러닝화 컬렉션" => vec!["러닝화", "패딩", "플리스"],
        "패딩 컬렉션" => vec!["패딩", "러닝화", "플리스"],
        "플리스 컬렉션" => vec!["플리스", "패딩", "러닝화"],
        "트레이닝 컬렉션" => vec!["러닝화", "플리스", "패딩"],
        other => vec![other, "패딩", "플리스"],
    };
    let items: String = order.iter().map(|l| link(l)).collect();
    page(&format!(
        "<nav><ul>{items}</ul></nav>\
         <section><h2>학생 할인</h2><p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>\
         <p>30일마다 인증 갱신 시, 할인 쿠폰이 <b>재발급</b>됩니다.</p><p>대상 상품 최대 12개월</p></section>\
         <section><h2>멤버십</h2><p>멤버십 회원은 구매 금액의 5% 적립</p></section>"
    ))
}

async fn stored_digests(pool: &PgPool, f: &Fixture) -> Vec<Option<String>> {
    sqlx::query_scalar(
        "SELECT anchors->'fingerprint'->>'page' FROM corporate_offer_facts WHERE program_id=$1 ORDER BY id",
    )
    .bind(f.program_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The 10-20 prod failure: an offer whose page changed outside the offer
/// (a menu, another section) was hidden for good. Its facts are current on
/// their own evidence: it stays shown on the changed page, the change is
/// recorded as telemetry only, and the stored anchors follow the new page.
/// A changed value on that page still hides it.
#[tokio::test]
async fn an_offer_whose_page_changed_elsewhere_stays_shown_and_a_changed_value_hides_it() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(sectioned_nike("러닝화 컬렉션"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let before = values(&pool, &f).await;

    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    let first = stored_digests(&pool, &f).await;

    // The navigation changed: still confirmed, the change recorded.
    site.page.lock().unwrap().1 = sectioned_nike("패딩 컬렉션");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    assert!(state(&pool, &f).await.0, "still shown");
    let detail = last_detail(&pool, &f).await;
    assert_eq!(detail["page_changed"], true, "{detail}");
    let new_digest = detail["page_digest"].as_str().unwrap().to_string();
    assert_ne!(first, vec![Some(new_digest.clone()); 2]);
    assert_eq!(stored_digests(&pool, &f).await, vec![Some(new_digest); 2]);
    assert_eq!(fact_statuses(&pool, &f).await, ["published", "published"]);
    assert_eq!(values(&pool, &f).await, before, "no value written");
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert!(
        metrics["resilience_24h"]["confirmed_on_changed_page"]
            .as_i64()
            .unwrap()
            >= 1,
        "{metrics}"
    );
    // The same page again: nothing changed.
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_detail(&pool, &f).await["page_changed"], false);

    // A changed value on the changed page hides the offer.
    site.page.lock().unwrap().1 = sectioned_nike("패딩 컬렉션").replace("10% 할인", "15% 할인");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("quote_missing".into(), true)
    );
    assert!(!state(&pool, &f).await.0, "hidden");
    assert_eq!(values(&pool, &f).await, before);
    cleanup(&pool, &f).await;
}

// ── re-baseline from fresh extraction ─────────────────────────────────────

fn fresh<'a>(html: &'a str, quote: &'a str, hours: i64, agree: bool) -> FreshExtraction<'a> {
    let last = Utc::now();
    FreshExtraction {
        html,
        url: "https://cafe.example/benefit",
        quote,
        readers_agree: agree,
        first_read_at: last - chrono::Duration::hours(hours),
        last_read_at: last,
    }
}

/// A fact held for a changed section returns once fresh extraction
/// re-derives it (same quote and value, readers agreeing, stable 24 h); it
/// stays held when the value, the quote, the agreement or the stability
/// differ.
#[test]
fn fresh_extraction_rebaselines_only_the_same_fact() {
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let before = nike_page("10", "30", "12");
    let page = resilience::PageView::new(&before, &visible_text(&before));
    let window = PageEvidence::from_html(&before)
        .window_signature(quote)
        .unwrap();
    let anchors = Anchors {
        fingerprint: resilience::capture(&page, quote),
        ..Anchors::default()
    };
    let stored = StoredFact {
        quote,
        baseline: Some(&window),
        ages: no_ages(),
        adoption_open: false,
        anchors: &anchors,
    };
    let held = before.replace("</main>", "<p>신학기 혜택 안내를 확인하세요</p></main>");
    let on = |html: &str, anchors: &Anchors| {
        resilience::resolve(
            &resilience::PageView::new(html, &visible_text(html)),
            &StoredFact {
                anchors,
                ..stored.clone()
            },
        )
    };
    assert_eq!(
        on(&held, &anchors),
        Resolution::Ambiguous(resilience::SECTION_CHANGED.into())
    );
    // Rebaselined: the held page becomes the fact's baseline and confirms.
    let new = fresh_anchors(&stored, &fresh(&held, quote, 25, true)).expect("rebaselined");
    assert!(on(&held, &new).confirms(), "{:?}", on(&held, &new));
    // Refused, each for its reason.
    let changed = held.replace("10% 할인", "15% 할인");
    for (case, reason) in [
        (fresh(&held, quote, 23, true), "not_stable"),
        (fresh(&held, quote, 25, false), "readers_disagree"),
        (
            fresh(
                &changed,
                "학생 인증 후 15% 할인 혜택을 제공해 드립니다.",
                25,
                true,
            ),
            "quote_differs",
        ),
        (fresh(&changed, quote, 25, true), "quote_missing"),
        (
            fresh(
                &held.replace("대상 상품 최대 12개월", "대상 상품 최대 24개월"),
                quote,
                25,
                true,
            ),
            "value_differs",
        ),
    ] {
        assert_eq!(fresh_anchors(&stored, &case).err(), Some(reason));
    }
}

/// P2's entry point on the database: refused leaves the offer held;
/// rebaselined, the next pass confirms and restores it.
#[tokio::test]
async fn rebaseline_from_fresh_returns_a_held_offer_through_the_next_pass() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));

    let held = nike_page("10", "30", "12")
        .replace("</main>", "<p>신학기 혜택 안내를 확인하세요</p></main>");
    site.page.lock().unwrap().1 = held.clone();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("ambiguous".into(), true));
    assert!(!state(&pool, &f).await.0, "held");

    let facts: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, quote FROM corporate_offer_facts WHERE program_id=$1 ORDER BY id",
    )
    .bind(f.program_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (id, quote) in &facts {
        assert_eq!(
            rebaseline_from_fresh(&pool, *id, &fresh(&held, quote, 3, true))
                .await
                .unwrap(),
            FreshOutcome::Refused("not_stable")
        );
    }
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(!state(&pool, &f).await.0, "still held");
    for (id, quote) in &facts {
        assert_eq!(
            rebaseline_from_fresh(&pool, *id, &fresh(&held, quote, 25, true))
                .await
                .unwrap(),
            FreshOutcome::Rebaselined
        );
    }
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
    assert!(
        state(&pool, &f).await.0,
        "restored by the normal confirmation"
    );
    cleanup(&pool, &f).await;
}

/// Re-baselining reads only the text added since the last confirmation:
/// text judged then (an old FAQ line) never blocks the way back, a new
/// ending does. A fact confirmed before section sentences were kept has no
/// record of what was judged, so its whole section is read the first time.
#[test]
fn rebaselining_reads_only_text_added_since_the_last_confirmation() {
    let quote = "학생 인증 후 10% 할인 혜택을 제공해 드립니다.";
    let before = nike_page("10", "30", "12").replace(
        "</main>",
        "<p>졸업생은 학생 할인 신청이 불가합니다.</p></main>",
    );
    let page = resilience::PageView::new(&before, &visible_text(&before));
    let window = PageEvidence::from_html(&before)
        .window_signature(quote)
        .unwrap();
    let anchors = Anchors {
        fingerprint: resilience::capture(&page, quote),
        ..Anchors::default()
    };
    let stored = StoredFact {
        quote,
        baseline: Some(&window),
        ages: no_ages(),
        adoption_open: false,
        anchors: &anchors,
    };
    let added = |line: &str| before.replace("</main>", &format!("<p>{line}</p></main>"));
    let news = added("신학기 혜택 안내를 확인하세요");
    assert!(fresh_anchors(&stored, &fresh(&news, quote, 25, true)).is_ok());
    let ended = added("본 혜택은 종료되었습니다.");
    assert_eq!(
        fresh_anchors(&stored, &fresh(&ended, quote, 25, true)).err(),
        Some("not_current")
    );
    let legacy = Anchors::default();
    assert_eq!(
        fresh_anchors(
            &StoredFact {
                anchors: &legacy,
                ..stored.clone()
            },
            &fresh(&news, quote, 25, true)
        )
        .err(),
        Some("not_current"),
        "no record of the judged text: the old FAQ line is read and holds it"
    );
}

/// The way back from a hold is off unless `CORPORATE_EVIDENCE_WAY_BACK_ENABLED`
/// is set (release review 2026-10-28: it re-baselined pause, refusal and
/// exclusion banners). Off, a held fact stays held, as in production before
/// it; on, a pass that holds a fact for a changed section records the new
/// section and, once it has stood a day and the second reader agrees,
/// re-baselines the fact on it. A new ending in that section keeps it held.
#[tokio::test]
async fn a_held_offer_returns_by_itself_only_while_the_way_back_is_on() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let backdate = |pool: PgPool, program: Uuid| async move {
        sqlx::query(
            "UPDATE corporate_offer_facts SET anchors = jsonb_set(anchors, '{pending_section,since}',
                 to_jsonb(now() - interval '25 hours'))
             WHERE program_id = $1 AND anchors ? 'pending_section'",
        )
        .bind(program)
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected()
    };
    for (way_back, line, returns) in [
        (false, "신학기 혜택 안내를 확인하세요", false),
        (true, "신학기 혜택 안내를 확인하세요", true),
        (true, "본 혜택은 종료되었습니다.", false),
    ] {
        if way_back {
            std::env::set_var(super::WAY_BACK_FLAG, "1");
        } else {
            std::env::remove_var(super::WAY_BACK_FLAG);
        }
        let site = site_with(nike_page("10", "30", "12"));
        let url = serve(site.clone()).await;
        let f = fixture(&pool, &url).await;
        pass(&pool, &f, VerifyMode::Live).await;
        assert_eq!(last_outcome(&pool, &f).await, ("confirmed".into(), true));
        site.page.lock().unwrap().1 =
            nike_page("10", "30", "12").replace("</main>", &format!("<p>{line}</p></main>"));
        pass(&pool, &f, VerifyMode::Live).await;
        assert!(!state(&pool, &f).await.0, "{line}: held");
        // A day later, the same section still there.
        let recorded = backdate(pool.clone(), f.program_id).await > 0;
        assert_eq!(
            recorded, way_back,
            "{line}: the new section is recorded only with the way back on"
        );
        pass(&pool, &f, VerifyMode::Live).await;
        assert_eq!(
            state(&pool, &f).await.0,
            returns,
            "way back {way_back}: {line}"
        );
        cleanup(&pool, &f).await;
    }
    std::env::remove_var(super::WAY_BACK_FLAG);
}

// ── data fixes of the 2026-10-26 health audit ─────────────────────────────

const FIXES_20261026: &str =
    include_str!("../../../../infra/migrations/20261028000102_corporate_audit_20261026_fixes.sql");

fn fixes_20261026() -> Vec<Value> {
    let start = FIXES_20261026.find("$spec$").expect("spec start") + "$spec$".len();
    let end = start + FIXES_20261026[start..].find("$spec$").expect("spec end");
    serde_json::from_str(&FIXES_20261026[start..end]).expect("spec is JSON")
}

/// One fix against the official page it was read from (saved under
/// `tests/fixtures/audit_20261026`): every anchor is that page's text,
/// every number written is in its anchor, every age is the anchor's.
fn validate_fix_20261026(fix: &Value) -> Result<(), String> {
    if fix["kind"] == "retire_duplicate" {
        let ids = fix["programmes"].as_array().ok_or("no programmes")?;
        return (ids.len() == 2 && ids[0] != ids[1])
            .then_some(())
            .ok_or_else(|| "a duplicate needs two programmes".into());
    }
    let file = fix["page_file"].as_str().ok_or("no page file")?;
    let html = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/audit_20261026")
            .join(file),
    )
    .map_err(|e| e.to_string())?;
    let page = PageEvidence::from_html(&html);
    let on_page = |anchor: &str| {
        page.contains(anchor)
            .then_some(())
            .ok_or_else(|| format!("anchor {anchor:?} is not text of {file}"))
    };
    if fix["kind"] == "age_and_summary" {
        let anchor = fix["anchor"].as_str().ok_or("no anchor")?;
        on_page(anchor)?;
        validate_age_fix(
            anchor,
            fix["set"]["min_age"].as_i64().map(|v| v as i32),
            fix["set"]["max_age"].as_i64().map(|v| v as i32),
        )?;
    }
    for replacement in fix["replacements"].as_array().ok_or("no replacements")? {
        let anchor = replacement["anchor"].as_str().ok_or("no anchor")?;
        on_page(anchor)?;
        validate_replacement(replacement["to"].as_str().ok_or("no text")?, anchor)?;
    }
    Ok(())
}

#[test]
fn fixes_20261026_pass_the_verbatim_validator() {
    let spec = fixes_20261026();
    assert_eq!(spec.len(), 3);
    for fix in &spec {
        validate_fix_20261026(fix).unwrap_or_else(|e| panic!("{}: {e}", fix["key"]));
    }
    // Mutations are refused: another age, a number the anchor lacks, an
    // anchor the page does not state.
    let age = spec.iter().find(|f| f["key"] == "spotify_age").unwrap();
    let mut other_age = age.clone();
    other_age["set"]["min_age"] = json!(19);
    assert!(validate_fix_20261026(&other_age).is_err());
    let mut other_text = age.clone();
    other_text["replacements"][0]["to"] =
        json!("만 19세 이상으로 대상 대학 교육기관에 등록된 학생은");
    assert!(validate_fix_20261026(&other_text).is_err());
    let sketch = spec
        .iter()
        .find(|f| f["key"] == "sketch_exclusions")
        .unwrap();
    let mut prose = sketch.clone();
    prose["replacements"][0]["anchor"] = json!("startups and businesses are not eligible");
    assert!(validate_fix_20261026(&prose).is_err());
}

/// The migration on the rows it was written for: the duplicate retired,
/// a user's bookmark moved to the kept row (unless the user has one there),
/// both summaries fixed, every change backed up, and a second run a no-op.
#[tokio::test]
async fn fixes_20261026_apply_once_and_keep_every_user_row() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let (kept, dup, sketch): (Uuid, Uuid, Uuid) = (
        "0daf688b-691c-44d6-9e5a-f3ea9f481a34".parse().unwrap(),
        "7af27e95-3a01-41f6-8639-6a9585cec9b9".parse().unwrap(),
        "1249aeda-db5b-46a5-b89e-3dc1005f9be8".parse().unwrap(),
    );
    let clean = |pool: PgPool| async move {
        sqlx::query("DELETE FROM corporate_audit_fix_backup WHERE fix_key LIKE '20261026:%'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![kept, dup, sketch])
            .execute(&pool)
            .await
            .unwrap();
    };
    clean(pool.clone()).await;
    let insert = |id: Uuid, title: &str, summary: &str, min_age: Option<i32>, url: &str| {
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions,min_age)
                     VALUES($1,'corporate_benefit','corporate_official',$2,$3,$4,'픽스처',$5,'always_open','corporate','discount',true,'current',ARRAY[]::text[],$6)")
            .bind(id).bind(format!("fixture:{id}")).bind(title.to_string()).bind(summary.to_string())
            .bind(url.to_string()).bind(min_age)
    };
    let spotify = "https://www.spotify.com/kr-ko/student/";
    insert(kept, "Spotify Premium 학생 요금제", "대상 대학 교육기관의 학생은 인증 후 Premium 학생 요금제를 이용할 수 있습니다. 첫 달 무료는 이전 Premium 체험 이력이 없는 대상자에게 적용되며 이후에는 유료로 갱신됩니다.", None, spotify)
        .execute(&pool).await.unwrap();
    insert(dup, "Spotify Premium 학생 요금제", "만 18세 이상으로 승인된 대학 교육기관에 재학 중인 학생은 Premium을 월 6,600원(부가세 포함)에 이용할 수 있습니다.", Some(18), spotify)
        .execute(&pool).await.unwrap();
    insert(sketch, "Sketch Education 무료 워크스페이스", "적격 학생은 Sketch Education 워크스페이스를 무료로 신청할 수 있습니다. Mac 앱을 사용하려면 지원되는 macOS 기기가 필요하며 독학과 창업·사업체 용도는 대상이 아닙니다.", None, "https://www.sketch.com/education/")
        .execute(&pool).await.unwrap();
    let user = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Uuid>("INSERT INTO users(auth_provider,auth_provider_id,role) VALUES('test',$1,'user') RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    // One user bookmarked only the duplicate; another bookmarked both.
    let (only_dup, both) = (user(pool.clone()).await, user(pool.clone()).await);
    for (u, p) in [(only_dup, dup), (both, dup), (both, kept)] {
        sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
            .bind(u)
            .bind(p)
            .execute(&pool)
            .await
            .unwrap();
    }

    sqlx::raw_sql(FIXES_20261026).execute(&pool).await.unwrap();
    let row = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (bool, Option<String>, Option<String>, String, Option<i32>)>(
                "SELECT is_active, retirement_reason, normalized_payload->>'duplicate_of', summary, min_age FROM programs WHERE id=$1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let retired = row(dup).await;
    assert_eq!(
        (retired.0, retired.1.as_deref(), retired.2.as_deref()),
        (false, Some("duplicate"), Some(kept.to_string().as_str()))
    );
    let kept_row = row(kept).await;
    assert!(kept_row.0);
    assert!(
        kept_row
            .3
            .starts_with("만 18세 이상으로 대상 대학 교육기관에 등록된 학생은"),
        "{}",
        kept_row.3
    );
    assert_eq!(kept_row.4, Some(18));
    assert!(row(sketch)
        .await
        .3
        .ends_with("독학·홈러닝, 비영리 단체, 교육기관의 마케팅 부서는 대상이 아닙니다."));
    let bookmarks: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT user_id, program_id FROM user_bookmarks WHERE user_id = ANY($1) ORDER BY user_id, program_id",
    )
    .bind(vec![only_dup, both])
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut expected = vec![(only_dup, kept), (both, kept), (both, dup)];
    expected.sort();
    assert_eq!(bookmarks, expected, "moved where allowed, every row kept");
    let backups: Vec<String> = sqlx::query_scalar(
        "SELECT fix_key FROM corporate_audit_fix_backup WHERE fix_key LIKE '20261026:%' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        backups,
        [
            "20261026:sketch_exclusions",
            "20261026:spotify_age",
            "20261026:spotify_duplicate",
            "20261026:spotify_duplicate:user_bookmarks"
        ]
    );
    // A second run changes nothing.
    let before = (row(kept).await, row(dup).await, row(sketch).await);
    sqlx::raw_sql(FIXES_20261026).execute(&pool).await.unwrap();
    assert_eq!((row(kept).await, row(dup).await, row(sketch).await), before);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![only_dup, both])
        .execute(&pool)
        .await
        .unwrap();
    clean(pool).await;
}

/// The census contract accepts every probed direct row (policy
/// `CENSUS_DIRECT_SQL`). A census written before the census followed
/// same-host hops rated a redirecting URL `direct:redirect_same_host`; the
/// evidence job then refused it as `policy_blocked` until the next weekly
/// census (production: SKT m.tworld), and promotion skipped it (AWS
/// Educate). Both now read it through the shared redirect resolver.
#[tokio::test]
async fn a_census_row_for_a_redirecting_url_does_not_wait_for_the_next_census() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    route(&site, "/benefit", 302, "", Some("/students"));
    route(&site, "/students", 200, &nike_page("10", "30", "12"), None);
    let f = fixture(&pool, &url).await;
    sqlx::query(
        "UPDATE corporate_census_entries SET reason='redirect_same_host' WHERE candidate_id=$1",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    let outcome = last_outcome(&pool, &f).await.0;
    assert_ne!(outcome, "policy_blocked");
    assert_eq!(outcome, "url_move_proposed", "read through the redirect");

    sqlx::query("UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    let mode: String = sqlx::query_scalar(
        "SELECT verification_mode FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mode, "evidence_auto");

    // An unprobed direct row is no verdict (F1, `LaneReason::is_verdict`):
    // promotion does not wait for it, and the reader rates the page itself
    // before it reads anything (this short page is a script shell to it).
    sqlx::query("UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE corporate_census_entries SET reason='robots_allowed_unprobed', probed=false WHERE candidate_id=$1")
        .bind(f.candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    let mode: String = sqlx::query_scalar(
        "SELECT verification_mode FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mode, "evidence_auto");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    cleanup(&pool, &f).await;
}

/// A census row written while our own pause kept the census from looking
/// (`domain_paused`, no request made) is no verdict on the URL: the last
/// real rating stands (production 2026-11-04, Apple Music: our own 429
/// back-off paused support.apple.com, the 10-31 census recorded
/// `domain_paused`, and the evidence job held a live offer as
/// policy-blocked under a hold no automation lifts). A real finding in a
/// newer census, robots disallowing the page, still blocks.
#[tokio::test]
async fn a_census_row_our_own_pause_wrote_is_no_verdict() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    // The fixture's direct rating is two days old; newer runs follow it,
    // never dated in the future (a future run would outrank other tests').
    sqlx::query(
        "UPDATE corporate_census_runs SET started_at = now() - interval '2 days', finished_at = now() - interval '2 days'
         WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1)",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let newer_run = |minutes: i32,
                     lane: &'static str,
                     reason: &'static str,
                     probed: bool,
                     robots: &'static str| {
        let pool = pool.clone();
        let url = url.clone();
        async move {
            let run: Uuid = sqlx::query_scalar(
                "INSERT INTO corporate_census_runs(status, started_at, finished_at)
                 VALUES('completed', now() - make_interval(mins => $1), now() - make_interval(mins => $1)) RETURNING id",
            )
            .bind(minutes)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
                 VALUES($1,'monitor',$2,$3,$4,$5,$6,$7,$8,$9,'none')",
            )
            .bind(run).bind(f.domain_id).bind(f.candidate_id).bind(f.program_id).bind(&url)
            .bind(lane).bind(reason).bind(probed).bind(robots)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    // The newest census only saw our own pause: the earlier direct rating
    // stands and the page is read.
    newer_run(60, "impossible", "domain_paused", false, "domain_paused").await;
    pass(&pool, &f, VerifyMode::Live).await;
    let outcome = last_outcome(&pool, &f).await.0;
    assert_eq!(outcome, "confirmed", "a pause of our own is no verdict");

    // A newer census that finds robots disallowing the page blocks it.
    newer_run(1, "snippet", "robots_disallow", false, "ok").await;
    sqlx::query(
        "UPDATE corporate_evidence_targets SET next_check_at = now() WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    sqlx::query("DELETE FROM corporate_census_runs WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1)")
        .bind(f.candidate_id)
        .execute(&pool)
        .await
        .ok();
    cleanup(&pool, &f).await;
}

/// When only the second Spotify row has user references, the duplicate fix
/// keeps that row and retires 0daf688b; the age fix written for 0daf688b
/// lands on the row kept in its place (the backup's `duplicate_of`), never
/// on the retired one.
#[tokio::test]
async fn fixes_20261026_fix_the_row_the_duplicate_fix_keeps() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let (first, second): (Uuid, Uuid) = (
        "0daf688b-691c-44d6-9e5a-f3ea9f481a34".parse().unwrap(),
        "7af27e95-3a01-41f6-8639-6a9585cec9b9".parse().unwrap(),
    );
    let clean = |pool: PgPool| async move {
        sqlx::query("DELETE FROM corporate_audit_fix_backup WHERE fix_key LIKE '20261026:%'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![first, second])
            .execute(&pool)
            .await
            .unwrap();
    };
    clean(pool.clone()).await;
    let summary = "대상 대학 교육기관의 학생은 인증 후 Premium 학생 요금제를 이용할 수 있습니다.";
    for id in [first, second] {
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
                     VALUES($1,'corporate_benefit','corporate_official',$2,'Spotify Premium 학생 요금제',$3,'픽스처','https://www.spotify.com/kr-ko/student/','always_open','corporate','discount',true,'current',ARRAY[]::text[])")
            .bind(id).bind(format!("fixture:{id}")).bind(summary)
            .execute(&pool).await.unwrap();
    }
    let user: Uuid = sqlx::query_scalar("INSERT INTO users(auth_provider,auth_provider_id,role) VALUES('test',$1,'user') RETURNING id")
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::raw_sql(FIXES_20261026).execute(&pool).await.unwrap();
    let row = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (bool, String, Option<i32>)>(
                "SELECT is_active, summary, min_age FROM programs WHERE id=$1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (kept, retired) = (row(second).await, row(first).await);
    assert!(kept.0 && !retired.0, "{kept:?} {retired:?}");
    assert!(
        kept.1
            .starts_with("만 18세 이상으로 대상 대학 교육기관에 등록된 학생은"),
        "{}",
        kept.1
    );
    assert_eq!(kept.2, Some(18));
    assert_eq!(
        (retired.1.as_str(), retired.2),
        (summary, None),
        "the retired row is left as it was"
    );
    let age_fix: Option<Uuid> = sqlx::query_scalar(
        "SELECT program_id FROM corporate_audit_fix_backup WHERE fix_key = '20261026:spotify_age'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(age_fix, Some(second));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    clean(pool).await;
}

// ── re-verification outcome classes (reverify): grace, backoff, incidents ─

/// Moves the offer's last evidence confirmation `hours` into the past.
async fn age_confirmation(pool: &PgPool, f: &Fixture, hours: i32) {
    sqlx::query(
        "UPDATE program_source_monitors SET evidence_verified_at = now() - make_interval(hours => $2)
         WHERE program_id = $1",
    )
    .bind(f.program_id)
    .bind(hours)
    .execute(pool)
    .await
    .unwrap();
}

async fn open_hold(pool: &PgPool, f: &Fixture) -> Option<String> {
    sqlx::query_scalar(
        "SELECT hold_reason FROM program_source_holds WHERE program_id=$1 AND resolved_at IS NULL",
    )
    .bind(f.program_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The grace incident's status, if one was ever opened.
async fn grace_incident_status(pool: &PgPool, f: &Fixture) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint=$1")
        .bind(format!("corporate_reverify:{}", f.candidate_id))
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn last_details(pool: &PgPool, f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT details FROM corporate_evidence_checks WHERE candidate_id=$1 ORDER BY checked_at DESC, id LIMIT 1")
        .bind(f.candidate_id).fetch_one(pool).await.unwrap()
}

async fn hours_to_next_check(pool: &PgPool, f: &Fixture) -> f64 {
    sqlx::query_scalar("SELECT (extract(epoch FROM next_check_at - now())/3600)::float8 FROM corporate_evidence_targets WHERE candidate_id=$1")
        .bind(f.candidate_id).fetch_one(pool).await.unwrap()
}

/// Transient: a 5xx keeps a confirmed offer shown inside its 72 h grace
/// window and is retried within hours; once the window ran out it is hidden
/// with a reversible hold and one incident, and the next confirmation
/// brings it back and closes the incident. No person is involved.
#[tokio::test]
async fn a_transient_failure_waits_out_its_grace_window_then_hides_and_heals() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);

    *site.page.lock().unwrap() = (503, "down for maintenance".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("fetch_failed".into(), false)
    );
    assert!(
        state(&pool, &f).await.0,
        "inside grace the offer stays shown"
    );
    let details = last_details(&pool, &f).await;
    assert_eq!(
        (details["class"].as_str(), details["in_grace"].as_bool()),
        (Some("transient"), Some(true))
    );
    let next = hours_to_next_check(&pool, &f).await;
    assert!(next < 5.0, "retried within hours, not days: {next} h");

    age_confirmation(&pool, &f, 73).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("fetch_failed".into(), true));
    assert!(!state(&pool, &f).await.0, "grace ran out: hidden");
    assert_eq!(
        open_hold(&pool, &f).await.as_deref(),
        Some("source_unreachable")
    );
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("open")
    );

    *site.page.lock().unwrap() = (200, nike_page("10", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    let (active, _, monitor, holds, _, _) = state(&pool, &f).await;
    assert_eq!((active, monitor.as_str(), holds), (true, "current", 0));
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("resolved")
    );
    cleanup(&pool, &f).await;
}

/// The grace window ends on schedule: a check is due by the hour the
/// window closes even when the backoff would wait longer (a 429 waits a
/// day, here two hours of grace are left).
#[tokio::test]
async fn an_offer_in_grace_is_checked_again_by_the_end_of_its_window() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    age_confirmation(&pool, &f, 70).await;
    *site.page.lock().unwrap() = (429, "slow down".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("rate_limited".into(), false)
    );
    let next = hours_to_next_check(&pool, &f).await;
    assert!(
        next <= 3.0,
        "due when the window closes, not a day later: {next} h"
    );
    cleanup(&pool, &f).await;
}

/// Blocked: robots disallowing the page or a bot wall keeps a confirmed
/// offer shown inside grace; past it the offer is hidden as access revoked
/// with an incident, and a later confirmation (robots allow us again)
/// lifts that hold by itself.
#[tokio::test]
async fn a_block_waits_out_its_grace_window_and_a_confirmation_lifts_it() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: MazimiBenefitBot\nDisallow: /benefit\n\nUser-agent: *\nAllow: /\n".into(),
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), false)
    );
    assert!(
        state(&pool, &f).await.0,
        "a block inside grace hides nothing"
    );
    let next = hours_to_next_check(&pool, &f).await;
    assert!(
        (23.0..=31.0).contains(&next),
        "a block is looked at daily in grace: {next} h"
    );

    age_confirmation(&pool, &f, 73).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), true)
    );
    assert!(!state(&pool, &f).await.0);
    assert_eq!(
        open_hold(&pool, &f).await.as_deref(),
        Some("access_revoked")
    );
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("open")
    );

    *site.robots.lock().unwrap() = (200, "User-agent: *\nAllow: /\n".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    assert!(
        state(&pool, &f).await.0,
        "the confirmation proves access is back"
    );
    assert_eq!(open_hold(&pool, &f).await, None);
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("resolved")
    );

    // A challenge page is a block too: shown inside grace.
    *site.page.lock().unwrap() = (403, "<title>Just a moment...</title>".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("blocked".into(), false));
    assert!(state(&pool, &f).await.0);
    cleanup(&pool, &f).await;
}

/// KT Y덤 (2026-11-10): robots.txt names the crawlers that may come and
/// ours is not one. A shown offer there gets no page request: it is a
/// robots block like any other, shown inside grace and hidden as access
/// revoked past it.
#[tokio::test]
async fn a_named_agent_allowlist_sends_no_page_request_and_waits_out_grace() {
    use std::sync::atomic::Ordering;
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n".into(),
    );
    let hits = site.page_hits.load(Ordering::SeqCst);
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        site.page_hits.load(Ordering::SeqCst),
        hits,
        "no page request"
    );
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), false)
    );
    assert!(state(&pool, &f).await.0, "shown inside grace");
    age_confirmation(&pool, &f, 73).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        site.page_hits.load(Ordering::SeqCst),
        hits,
        "no page request"
    );
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), true)
    );
    assert!(!state(&pool, &f).await.0, "hidden past grace");
    assert_eq!(
        open_hold(&pool, &f).await.as_deref(),
        Some("access_revoked")
    );
    cleanup(&pool, &f).await;
}

/// Refuted hides at once, grace or not: the page itself says otherwise.
#[tokio::test]
async fn a_refutation_hides_at_once_inside_grace() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    site.page.lock().unwrap().1 = nike_page("10", "30", "24");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("window_changed".into(), true)
    );
    assert!(!state(&pool, &f).await.0);
    assert_eq!(
        reextract_of(&pool, &f).await.as_deref(),
        Some("window_changed")
    );
    // A redirect off the site is refuted as well.
    site.page.lock().unwrap().1 = nike_page("10", "30", "12");
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    route(
        &site,
        "/benefit",
        302,
        "",
        Some("https://elsewhere.example/landing"),
    );
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("redirected".into(), true));
    assert!(!state(&pool, &f).await.0);
    cleanup(&pool, &f).await;
}

/// Fallback evidence: robots now closes the page to us, and the site's
/// sitemap lists another official page stating the offer as stored. That
/// page confirms nothing on first sight: the offer waits out its grace
/// window as a block while the new URL waits for two agreeing sightings
/// 24 h apart, and is confirmed there only once the URL is adopted. A
/// redesign-tolerant (relocated) match on such a page counts for nothing,
/// and past grace a pending move hides the offer as access revoked.
#[tokio::test]
async fn a_page_closed_by_robots_is_confirmed_elsewhere_only_once_adopted() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let closed = |site: &Site, base: &str, fallback: String| {
        *site.robots.lock().unwrap() = (
            200,
            "User-agent: MazimiBenefitBot\nDisallow: /benefit\n\nUser-agent: *\nAllow: /\n".into(),
        );
        route(
            site,
            "/sitemap.xml",
            200,
            &format!("<?xml version=\"1.0\"?><urlset><url><loc>{base}/help/student-discount</loc></url></urlset>"),
            None,
        );
        route(site, "/help/student-discount", 200, &fallback, None);
    };

    // The same page elsewhere: shown in grace, then adopted and confirmed.
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    age_confirmation(&pool, &f, 24).await;
    closed(&site, &base, nike_page("10", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("url_move_proposed".into(), false)
    );
    assert!(state(&pool, &f).await.0, "waits inside grace");
    let confirmed_hours: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM now() - evidence_verified_at)/3600)::float8 FROM program_source_monitors WHERE program_id=$1")
        .bind(f.program_id).fetch_one(&pool).await.unwrap();
    assert!(confirmed_hours > 23.0, "a first sighting confirms nothing");
    backdate_proposals(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await, ("url_adopted".into(), true));
    assert!(state(&pool, &f).await.0);
    cleanup(&pool, &f).await;

    // A relocated match on the other page is no evidence: still blocked.
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    closed(&site, &base, redesigned_nike("10", "30"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("robots_disallowed".into(), false)
    );
    cleanup(&pool, &f).await;

    // A pending move past grace hides the offer.
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    age_confirmation(&pool, &f, 73).await;
    closed(&site, &base, nike_page("10", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("url_move_proposed".into(), true)
    );
    assert!(
        !state(&pool, &f).await.0,
        "grace ran out with the move pending"
    );
    assert_eq!(
        open_hold(&pool, &f).await.as_deref(),
        Some("access_revoked")
    );
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("open")
    );
    cleanup(&pool, &f).await;
}

/// A redirect is followed only within one registrable domain by the Public
/// Suffix List, never across tenants of a shared host.
#[test]
fn a_redirect_is_followed_only_within_one_registrable_domain() {
    let same = |a: &str, b: &str| {
        super::same_registrable(&Url::parse(a).unwrap(), &Url::parse(b).unwrap())
    };
    assert!(same(
        "https://www.apple.com/kr/music/",
        "https://music.apple.com/kr/student"
    ));
    assert!(same(
        "https://shop.example.co.kr/a",
        "https://www.example.co.kr/b"
    ));
    for (a, b) in [
        ("https://a.com.hk/x", "https://b.com.hk/x"),
        ("https://a.co.nz/x", "https://b.co.nz/x"),
        ("https://a.com.br/x", "https://b.com.br/x"),
        ("https://one.github.io/x", "https://two.github.io/x"),
        ("https://www.apple.com/x", "http://music.apple.com/x"),
        ("https://www.apple.com/x", "https://apple.example/x"),
        ("http://127.0.0.1:8080/x", "http://127.0.0.1:9090/x"),
    ] {
        assert!(!same(a, b), "{a} -> {b}");
    }
}

/// The grace incident has nothing left to wait for once the source is
/// refuted: a refuted page closes it. Our domain policy withdrawing the
/// source is its own outcome (`domain_withdrawn`), refuted and hidden now.
#[tokio::test]
async fn a_refutation_closes_the_grace_incident_and_a_withdrawn_domain_hides() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    pass(&pool, &f, VerifyMode::Live).await;
    *site.page.lock().unwrap() = (503, "down".into());
    age_confirmation(&pool, &f, 73).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("open")
    );
    *site.page.lock().unwrap() = (200, nike_page("10", "30", "24"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "window_changed");
    assert_eq!(
        grace_incident_status(&pool, &f).await.as_deref(),
        Some("resolved")
    );

    site.page.lock().unwrap().1 = nike_page("10", "30", "12");
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    sqlx::query("UPDATE benefit_source_domains SET collection_mode='blocked' WHERE id=$1")
        .bind(f.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("domain_withdrawn".into(), true)
    );
    assert!(!state(&pool, &f).await.0);
    assert_eq!(
        open_hold(&pool, &f).await.as_deref(),
        Some("access_revoked")
    );
    cleanup(&pool, &f).await;
}

/// Evidence taking over a current monitor starts its grace window: a first
/// 5xx right after promotion keeps the offer shown.
#[tokio::test]
async fn promotion_starts_the_grace_window_of_a_current_monitor() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query("UPDATE program_source_monitors SET verification_mode='human', verification_state='current', evidence_verified_at=NULL, review_due_at=now()+interval '10 days' WHERE program_id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE program_source_holds SET resolved_at=now(), resolution='dismissed' WHERE program_id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE programs SET is_active=true, program_status='always_open', source_health='current' WHERE id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    super::promote_monitors(&pool).await.unwrap();
    let (mode, seeded): (String, bool) = sqlx::query_as(
        "SELECT verification_mode, evidence_verified_at IS NOT NULL FROM program_source_monitors WHERE program_id=$1",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((mode.as_str(), seeded), ("evidence_auto", true));
    *site.page.lock().unwrap() = (503, "down".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("fetch_failed".into(), false)
    );
    assert!(
        state(&pool, &f).await.0,
        "a first 5xx after promotion hides nothing"
    );
    cleanup(&pool, &f).await;
}

/// The run alert: open while too many checks are unreadable, resolved by
/// the first run under the threshold; too few checks leave it alone.
#[tokio::test]
async fn the_run_health_alert_opens_and_resolves_itself() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let status = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM operational_events WHERE fingerprint=$1",
        )
        .bind(super::HEALTH_ALERT_FINGERPRINT)
        .fetch_optional(&pool)
        .await
        .unwrap()
    };
    sqlx::query("DELETE FROM operational_events WHERE fingerprint=$1")
        .bind(super::HEALTH_ALERT_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();
    let outage = RunHealth {
        confirmed: 1,
        transient: 3,
        in_grace: 3,
        ..RunHealth::default()
    };
    super::report_health(&pool, &outage).await.unwrap();
    assert_eq!(status().await.as_deref(), Some("open"));
    let few = RunHealth {
        confirmed: 1,
        ..RunHealth::default()
    };
    super::report_health(&pool, &few).await.unwrap();
    assert_eq!(
        status().await.as_deref(),
        Some("open"),
        "one check says nothing"
    );
    let calm = RunHealth {
        confirmed: 4,
        refuted: 1,
        ..RunHealth::default()
    };
    super::report_health(&pool, &calm).await.unwrap();
    assert_eq!(status().await.as_deref(), Some("resolved"));
    sqlx::query("DELETE FROM operational_events WHERE fingerprint=$1")
        .bind(super::HEALTH_ALERT_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();
}

/// Replay of the 2026-11-04 drop under the outcome classes, row by row
/// on the mock site. Apple Music (our fault: a 429 back-off, then a census
/// row our own pause wrote) stays shown and, holding the access_revoked
/// hold the old rule gave it, comes back by its next confirmation. The
/// rows whose page refutes the offer stay hidden: a changed value window
/// (GitHub Student Pack, 할리스) and a quote no longer on the page (Notion,
/// SKT 라이트 59, Unity's /kr/ page). The retired Spotify duplicate is not
/// re-verified at all.
#[tokio::test]
async fn the_1104_drop_replays_under_the_outcome_classes() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;

    // Apple Music.
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    *site.page.lock().unwrap() = (429, "slow down".into());
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("rate_limited".into(), false)
    );
    assert!(state(&pool, &f).await.0, "Apple Music: a 429 is no verdict");
    // The census then rated the URL other than direct (as 11-04's paused
    // row was read before the census fix): blocked, still in grace.
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_census_runs(status, started_at, finished_at)
         VALUES('completed', now() - interval '1 minute', now() - interval '1 minute') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE corporate_census_runs SET started_at = now() - interval '2 days', finished_at = now() - interval '2 days'
         WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1) AND id <> $2",
    )
    .bind(f.candidate_id)
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
         VALUES($1,'monitor',$2,$3,$4,$5,'snippet','robots_disallow',false,'ok','none')",
    )
    .bind(run).bind(f.domain_id).bind(f.candidate_id).bind(f.program_id).bind(&url)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(
        last_outcome(&pool, &f).await,
        ("policy_blocked".into(), false)
    );
    assert!(
        state(&pool, &f).await.0,
        "Apple Music: blocked inside grace stays shown"
    );
    // Production's state after 11-04: held as access_revoked by the old
    // rule. The census rates it direct again and the page reads as stored.
    sqlx::query("DELETE FROM corporate_census_runs WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'access_revoked',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE programs SET is_active=false, program_status='review_required' WHERE id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE program_source_monitors SET verification_state='change_detected' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    *site.page.lock().unwrap() = (200, nike_page("10", "30", "12"));
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    assert!(
        state(&pool, &f).await.0,
        "Apple Music returns by its confirmation"
    );
    assert_eq!(open_hold(&pool, &f).await, None);
    cleanup(&pool, &f).await;

    // Refuted rows stay hidden: (row, page, outcome).
    for (row, html, outcome) in [
        (
            "GitHub Student Pack / 할리스",
            nike_page("10", "30", "24"),
            "window_changed",
        ),
        (
            "Notion / SKT 라이트 59 / Unity",
            nike_page("15", "30", "12"),
            "quote_missing",
        ),
    ] {
        let site = site_with(nike_page("10", "30", "12"));
        let f = fixture(&pool, &serve(site.clone()).await).await;
        pass(&pool, &f, VerifyMode::Live).await;
        site.page.lock().unwrap().1 = html;
        pass(&pool, &f, VerifyMode::Live).await;
        assert_eq!(
            last_outcome(&pool, &f).await,
            (outcome.into(), true),
            "{row}"
        );
        assert!(!state(&pool, &f).await.0, "{row}");
        cleanup(&pool, &f).await;
    }

    // The retired Spotify duplicate: a retired monitor is never a target.
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query(
        "UPDATE program_source_monitors SET verification_state='retired' WHERE program_id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    let checks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM corporate_evidence_checks WHERE candidate_id=$1")
            .bind(f.candidate_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(checks, 0, "Spotify duplicate: retired, never re-verified");
    cleanup(&pool, &f).await;
}

// ── stale schedules: a wait set on a reading we no longer accept ─────────

/// One live pass over the source if it is due; the schedule is not forced.
async fn pass_if_due(pool: &PgPool, f: &Fixture) {
    let http = LoopbackHttp::new(UA);
    run_with(
        pool,
        VerifyMode::Live,
        &http,
        Some(vec![f.candidate_id]),
        Some(std::time::Duration::from_millis(20)),
    )
    .await
    .unwrap();
}

async fn set_schedule(pool: &PgPool, f: &Fixture, outcome: &str, class: Option<&str>) {
    sqlx::query(
        "UPDATE corporate_evidence_targets
         SET last_outcome = $2, schedule_class = $3, next_check_at = now() + interval '7 days'
         WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .bind(outcome)
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
}

async fn is_due(pool: &PgPool, f: &Fixture) -> bool {
    sqlx::query_scalar(
        "SELECT next_check_at <= now() FROM corporate_evidence_targets WHERE candidate_id=$1",
    )
    .bind(f.candidate_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Production 2026-11-05: Apple Music, held as access_revoked by a census
/// row our own pause wrote, waited a week (`policy_blocked`, retried weekly)
/// after the census fix already rated it direct again. A stored outcome the
/// current rules no longer reach is due at once: the next run reads the
/// page, confirms it and lifts the hold, with no manual step.
#[tokio::test]
async fn a_policy_block_the_census_no_longer_gives_is_due_at_once() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert!(state(&pool, &f).await.0);
    // Production's state: hidden as access_revoked, a week's wait.
    sqlx::query("INSERT INTO program_source_holds(program_id,candidate_id,hold_reason,previous_is_active,previous_program_status)
                 VALUES($1,$2,'access_revoked',true,'always_open')")
        .bind(f.program_id).bind(f.candidate_id).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE programs SET is_active=false, program_status='review_required' WHERE id=$1",
    )
    .bind(f.program_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE program_source_monitors SET verification_state='change_detected' WHERE program_id=$1")
        .bind(f.program_id).execute(&pool).await.unwrap();
    set_schedule(&pool, &f, "policy_blocked", None).await;

    pass_if_due(&pool, &f).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    let (active, _, monitor, holds, _, _) = state(&pool, &f).await;
    assert_eq!((active, monitor.as_str(), holds), (true, "current", 0));
    // Idempotent: nothing stale is left.
    assert_eq!(super::reschedule_stale(&pool).await.unwrap(), 0);
    cleanup(&pool, &f).await;
}

/// A block the census still gives keeps its wait; a domain our policy
/// admits again is due at once.
#[tokio::test]
async fn only_an_outcome_whose_input_changed_is_due_at_once() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    sqlx::query(
        "UPDATE corporate_census_runs SET started_at = now() - interval '2 days', finished_at = now() - interval '2 days'
         WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1)",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_census_runs(status, started_at, finished_at)
         VALUES('completed', now() - interval '1 minute', now() - interval '1 minute') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
         VALUES($1,'monitor',$2,$3,$4,$5,'snippet','robots_disallow',false,'ok','none')",
    )
    .bind(run).bind(f.domain_id).bind(f.candidate_id).bind(f.program_id).bind(&url)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO corporate_evidence_targets(candidate_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    set_schedule(&pool, &f, "policy_blocked", Some("blocked")).await;
    super::reschedule_stale(&pool).await.unwrap();
    assert!(!is_due(&pool, &f).await, "the census still blocks it");

    sqlx::query("DELETE FROM corporate_census_runs WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    set_schedule(&pool, &f, "domain_withdrawn", Some("refuted")).await;
    super::reschedule_stale(&pool).await.unwrap();
    assert!(is_due(&pool, &f).await, "the domain policy admits it again");
    cleanup(&pool, &f).await;
}

/// A label whose class changed since its schedule was set is due at once;
/// a schedule set before classes were recorded stands.
#[tokio::test]
async fn a_label_whose_class_changed_is_due_at_once() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let f = fixture(&pool, &serve(site.clone()).await).await;
    sqlx::query(
        "INSERT INTO corporate_evidence_targets(candidate_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    // `fetch_failed` is transient now; a schedule set when it read blocked
    // is stale.
    set_schedule(&pool, &f, "fetch_failed", Some("blocked")).await;
    super::reschedule_stale(&pool).await.unwrap();
    assert!(is_due(&pool, &f).await);
    set_schedule(&pool, &f, "fetch_failed", Some("transient")).await;
    super::reschedule_stale(&pool).await.unwrap();
    assert!(!is_due(&pool, &f).await, "same class: the wait stands");
    set_schedule(&pool, &f, "fetch_failed", None).await;
    super::reschedule_stale(&pool).await.unwrap();
    assert!(
        !is_due(&pool, &f).await,
        "no recorded class: the wait stands"
    );
    // Every scheduled check records its class.
    pass(&pool, &f, VerifyMode::Live).await;
    let class: Option<String> = sqlx::query_scalar(
        "SELECT schedule_class FROM corporate_evidence_targets WHERE candidate_id=$1",
    )
    .bind(f.candidate_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(class.as_deref(), Some("confirmed"));
    cleanup(&pool, &f).await;
}

// ── F1: one decision for "may this reader read the URL as direct HTML" ────

/// A page long enough to rate `direct_html` (more than [`policy::JS_SHELL_CHARS`]
/// visible characters) that states the Nike quote.
fn long_nike_page() -> String {
    let pad = "나이키 멤버십 학생 프로그램은 학생 인증을 마친 회원에게 제공되며 온라인 스토어에서 이용할 수 있습니다. "
        .repeat(10);
    page(&format!(
        "<h2>학생 할인</h2><p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>\
         <p>30일마다 인증 갱신 시, 할인 쿠폰이 <b>재발급</b>됩니다.</p><p>대상 상품 최대 12개월</p>\
         <p>{pad}</p>"
    ))
}

/// Drops every census row of the fixture: the census never rated its URL.
async fn unrated(pool: &PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM corporate_census_runs WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1)")
        .bind(f.candidate_id)
        .execute(pool)
        .await
        .unwrap();
}

/// A completed census run `minutes` ago with one monitor row for the fixture.
#[allow(clippy::too_many_arguments)]
async fn census_row(
    pool: &PgPool,
    f: &Fixture,
    url: &str,
    minutes: i32,
    lane: &str,
    reason: &str,
    probed: bool,
    robots: &str,
) {
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_census_runs(status, started_at, finished_at)
         VALUES('completed', now() - make_interval(mins => $1), now() - make_interval(mins => $1)) RETURNING id",
    )
    .bind(minutes)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
         VALUES($1,'monitor',$2,$3,$4,$5,$6,$7,$8,$9,'none')",
    )
    .bind(run)
    .bind(f.domain_id)
    .bind(f.candidate_id)
    .bind(f.program_id)
    .bind(url)
    .bind(lane)
    .bind(reason)
    .bind(probed)
    .bind(robots)
    .execute(pool)
    .await
    .unwrap();
}

async fn shown(pool: &PgPool, f: &Fixture) -> bool {
    sqlx::query_scalar("SELECT is_active FROM programs WHERE id=$1")
        .bind(f.program_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// RC1 (production 2026-11-10: 15 intake offers): a URL no census has
/// rated is read by the evidence job under its own policy session (opt-out,
/// robots for our agent on the path, the polite session, the lane rated by
/// `choose_lane`) instead of waiting a week as `policy_blocked`.
#[tokio::test]
async fn f1_a_url_no_census_rated_is_read_and_confirmed() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    assert!(shown(&pool, &f).await, "the confirmed offer is shown");
    assert!(site.page_hits.load(std::sync::atomic::Ordering::SeqCst) > 0);
    cleanup(&pool, &f).await;
}

/// Must refuse: robots disallowing the path for our agent stops the read
/// before any page request, census or not.
#[tokio::test]
async fn f1_an_unrated_url_robots_disallows_is_never_requested() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: *\nDisallow: /benefit\nDisallow: /sitemap.xml\n".into(),
    );
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "robots_disallowed");
    assert_eq!(site.page_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!shown(&pool, &f).await);
    cleanup(&pool, &f).await;
}

/// Review A-H2.1: robots closing an unrated URL answers `robots_disallowed`
/// without rediscovery, even with a sitemap we may read that lists another
/// page stating the offer: an unrated source has no confirmation to find
/// again, and a rediscovered page was read unrated.
#[tokio::test]
async fn h2_an_unrated_url_closed_by_robots_is_not_rediscovered() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    *site.robots.lock().unwrap() = (
        200,
        "User-agent: MazimiBenefitBot\nDisallow: /benefit\n\nUser-agent: *\nAllow: /\n".into(),
    );
    route(
        &site,
        "/sitemap.xml",
        200,
        &format!("<?xml version=\"1.0\"?><urlset><url><loc>{base}/help/student-discount</loc></url></urlset>"),
        None,
    );
    route(
        &site,
        "/help/student-discount",
        200,
        &long_nike_page(),
        None,
    );
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "robots_disallowed");
    assert_eq!(site.page_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!shown(&pool, &f).await);
    cleanup(&pool, &f).await;
}

/// Verify V-M3: a 404 on an unrated URL answers `gone` without
/// rediscovery, even with a readable sitemap that lists another page
/// stating the offer.
#[tokio::test]
async fn h2_an_unrated_gone_url_is_not_rediscovered() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let base = url.trim_end_matches("/benefit").to_string();
    *site.page.lock().unwrap() = (404, "not found".into());
    route(
        &site,
        "/sitemap.xml",
        200,
        &format!("<?xml version=\"1.0\"?><urlset><url><loc>{base}/help/student-discount</loc></url></urlset>"),
        None,
    );
    route(
        &site,
        "/help/student-discount",
        200,
        &long_nike_page(),
        None,
    );
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "gone");
    assert!(!shown(&pool, &f).await);
    cleanup(&pool, &f).await;
}

/// Verify V-M3 (review A-H2.3): a redirect to a sibling host of the same
/// registrable domain is followed only for a URL a census rated; never
/// for an unrated one, and never off the site.
#[test]
fn only_a_census_rated_url_follows_a_sibling_host_redirect() {
    let url = |s: &str| Url::parse(s).unwrap();
    let from = url("https://www.apple.com/kr/music/");
    let sibling = url("https://music.apple.com/kr/student");
    assert!(super::follows_off_host(true, &from, &sibling));
    assert!(!super::follows_off_host(false, &from, &sibling));
    assert!(!super::follows_off_host(
        true,
        &from,
        &url("https://elsewhere.example/landing")
    ));
}

/// Review A-H2.2: a stored terms signal the census refuses stops an
/// unrated URL before any page request.
#[tokio::test]
async fn h2_a_stored_refusing_terms_signal_stops_an_unrated_url_before_a_request() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO corporate_census_runs(status, started_at, finished_at)
         VALUES('completed', now() - interval '1 hour', now() - interval '1 hour') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,url,lane,reason,probed,robots_state,tos_signal)
         VALUES($1,'domain',$2,$3,'impossible','tos_specific_collection',false,'allowed','specific_collection')",
    )
    .bind(run)
    .bind(f.domain_id)
    .bind(&url)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    assert_eq!(site.page_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!shown(&pool, &f).await);
    sqlx::query("DELETE FROM corporate_census_runs WHERE id = $1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &f).await;
}

/// Review A-M6: the terms branch of the reader's own rating. An unrated
/// page whose terms prohibit collecting its events is read once and shown
/// to no one.
#[tokio::test]
async fn h2_an_unrated_page_whose_terms_prohibit_collection_is_never_shown() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site =
        site_with(long_nike_page().replace("<footer>", "<footer><a href=\"/terms\">이용약관</a> "));
    route(
        &site,
        "/terms",
        200,
        "<html><body><p>본 사이트의 이벤트 정보를 자동화된 프로그램으로 수집하여 상업적으로 이용하는 것을 금지합니다.</p></body></html>",
        None,
    );
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    assert!(!shown(&pool, &f).await);
    cleanup(&pool, &f).await;
}

/// Must refuse: the reader's own rating of an unrated URL is the census's
/// rule (`choose_lane`). A script shell is no direct HTML: nothing is read
/// as evidence and the offer stays hidden.
#[tokio::test]
async fn f1_an_unrated_script_shell_is_refused_by_the_readers_own_rating() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    assert!(!shown(&pool, &f).await);
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts WHERE program_id=$1 AND window_signature IS NOT NULL",
    )
    .bind(f.program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        facts, 0,
        "no baseline is taken from a page the lane refuses"
    );
    cleanup(&pool, &f).await;
}

/// RC1, Apple Music (production 2026-11-07): a 429 the census itself got,
/// or any other reply that says nothing about the page (robots unreachable,
/// a 5xx, a probe never made), is no verdict; the last real rating stands
/// and the confirmed offer is read and stays shown.
#[tokio::test]
async fn f1_a_confirmed_offer_survives_a_census_429() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    sqlx::query(
        "UPDATE corporate_census_runs SET started_at = now() - interval '2 days', finished_at = now() - interval '2 days'
         WHERE id IN (SELECT run_id FROM corporate_census_entries WHERE candidate_id = $1)",
    )
    .bind(f.candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");
    for (minutes, lane, reason, robots) in [
        (60, "impossible", "rate_limited", "rate_limited"),
        (50, "impossible", "robots_unavailable", "robots_unavailable"),
        (40, "impossible", "page_unreachable", "ok"),
        (30, "direct", "robots_allowed_unprobed", "ok"),
    ] {
        census_row(&pool, &f, &url, minutes, lane, reason, false, robots).await;
        pass(&pool, &f, VerifyMode::Live).await;
        assert_eq!(
            last_outcome(&pool, &f).await.0,
            "confirmed",
            "a census row `{reason}` is no verdict on the page"
        );
        assert!(shown(&pool, &f).await, "still shown after `{reason}`");
    }
    // A real finding still blocks: robots disallowing the page, a wall.
    census_row(
        &pool,
        &f,
        &url,
        1,
        "snippet",
        "robots_disallow",
        false,
        "ok",
    )
    .await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    cleanup(&pool, &f).await;
}

/// RC1 for promotion (production 2026-11-11: the SKT issuer offers): a
/// person's monitor whose programme carries facts moves to automatic
/// evidence while no census rated its URL otherwise than direct; the
/// evidence job then reads it under its own policy. A non-direct verdict
/// keeps it with its person.
#[tokio::test]
async fn f1_promotion_does_not_wait_for_a_census_that_never_rated_the_url() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let mode = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT verification_mode FROM program_source_monitors WHERE program_id=$1",
        )
        .bind(f.program_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    unrated(&pool, &f).await;
    sqlx::query("UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    assert_eq!(mode(pool.clone()).await, "evidence_auto");
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "confirmed");

    sqlx::query("UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    census_row(
        &pool,
        &f,
        &url,
        1,
        "snippet",
        "robots_disallow",
        false,
        "ok",
    )
    .await;
    promote_monitors(&pool).await.unwrap();
    assert_eq!(
        mode(pool.clone()).await,
        "human",
        "a robots verdict keeps it"
    );
    cleanup(&pool, &f).await;
}

/// Review A-M6 and A-M2: promotion reads only a domain our policy keeps
/// and never a candidate the door job owns (here, by a `door` monitor).
#[tokio::test]
async fn promotion_skips_a_rejected_domain_and_a_door_owned_candidate() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let mode = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT verification_mode FROM program_source_monitors WHERE program_id=$1",
        )
        .bind(f.program_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    unrated(&pool, &f).await;
    let human = |pool: PgPool| async move {
        sqlx::query(
            "UPDATE program_source_monitors SET verification_mode='human' WHERE program_id=$1",
        )
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    };
    human(pool.clone()).await;
    sqlx::query("UPDATE benefit_source_domains SET compliance_status='rejected' WHERE id=$1")
        .bind(f.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    assert_eq!(mode(pool.clone()).await, "human", "a rejected domain");
    sqlx::query("UPDATE benefit_source_domains SET compliance_status='pending' WHERE id=$1")
        .bind(f.domain_id)
        .execute(&pool)
        .await
        .unwrap();
    let door_program = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active) VALUES($1,'corporate_benefit','manual',$1::text,'도어 행','active',true)")
        .bind(door_program)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,is_primary,verification_state,verified_at,review_due_at,verification_mode)
                 VALUES($1,$2,false,'current',now(),now()+interval '30 days','door')")
        .bind(door_program)
        .bind(f.candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    // With a fact, so only the door ownership keeps it from promotion.
    sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,value,quote,quote_key,evidence_source,adopt_before)
                 SELECT $1, fact_type, value, quote, $2, evidence_source, adopt_before
                 FROM corporate_offer_facts WHERE program_id = $3 AND quote = $4")
        .bind(door_program)
        .bind(quote_key(door_program, "학생 인증 후 10% 할인 혜택을 제공해 드립니다."))
        .bind(f.program_id)
        .bind("학생 인증 후 10% 할인 혜택을 제공해 드립니다.")
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    assert_eq!(mode(pool.clone()).await, "human", "a door-owned candidate");
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(door_program)
        .execute(&pool)
        .await
        .unwrap();
    promote_monitors(&pool).await.unwrap();
    assert_eq!(mode(pool.clone()).await, "evidence_auto", "control");
    cleanup(&pool, &f).await;
}

/// A source the old census-only rule parked as `policy_blocked` (no census
/// verdict) is due at the next run; one the reader itself rated not direct
/// keeps its wait, so it is not fetched every run.
#[tokio::test]
async fn f1_a_census_wait_written_by_the_old_rule_is_due_now() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(nike_page("10", "30", "12"));
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    unrated(&pool, &f).await;
    pass(&pool, &f, VerifyMode::Live).await;
    assert_eq!(last_outcome(&pool, &f).await.0, "policy_blocked");
    sqlx::query("UPDATE corporate_evidence_targets SET next_check_at = now() + interval '7 days' WHERE candidate_id=$1")
        .bind(f.candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    super::reschedule_stale(&pool).await.unwrap();
    assert!(
        !is_due(&pool, &f).await,
        "the reader's own rating keeps its wait"
    );
    sqlx::query(
        "UPDATE corporate_evidence_checks SET details = jsonb_set(details, '{detail}', to_jsonb($2::text))
         WHERE candidate_id = $1",
    )
    .bind(f.candidate_id)
    .bind("no completed census within 30 days rates this URL direct HTML")
    .execute(&pool)
    .await
    .unwrap();
    super::reschedule_stale(&pool).await.unwrap();
    assert!(is_due(&pool, &f).await, "the old rule's wait is stale");
    cleanup(&pool, &f).await;
}

// ── F3: baselines carry the reader version that wrote them ───────────────

fn saved_real_page(file: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/real_pages")
            .join(file),
    )
    .expect("saved page")
}

fn fact_row(quote: &str, anchors: Value, window: Option<String>, open: bool) -> FactRow {
    FactRow {
        id: Uuid::new_v4(),
        program_id: Uuid::new_v4(),
        quote: quote.to_string(),
        anchors,
        window_signature: window,
        shadow_window_signature: None,
        adopt_before: Some(if open {
            Utc::now() + chrono::Duration::days(1)
        } else {
            Utc::now() - chrono::Duration::days(1)
        }),
        min_age: None,
        max_age: None,
    }
}

fn assess_on(html: &str, fact: &FactRow) -> Assessed {
    let view = PageView::at(html, &visible_text(html), super::super::health::kst_now());
    assess(&view, html, "https://example.test/page", fact, true, false)
}

/// The first confirmation of `quote` on `html`: its stored anchors and
/// window, as the live pass writes them.
fn first_confirmation(html: &str, quote: &str) -> (Value, String) {
    let first = assess_on(html, &fact_row(quote, json!({}), None, true));
    assert!(first.resolution.confirms(), "{:?}", first.resolution);
    let window = match &first.resolution {
        Resolution::Exact(FactOutcome::Confirmed {
            window_signature, ..
        }) => window_signature.clone(),
        other => panic!("{other:?}"),
    };
    (
        serde_json::to_value(first.anchors.expect("anchors")).unwrap(),
        super::stored_window(&window),
    )
}

/// RC3 replay (production 10-18/10-19: 커피빈, 예스24, 할리스 failed
/// `window_changed` on an identical page after a reader change): a value
/// window written by another reader version, on the same page bytes, is
/// re-baselined by this reader and confirms; it is never `window_changed`.
#[test]
fn a_byte_identical_page_is_never_failed_by_a_reader_upgrade() {
    let html = saved_real_page("247e7e73.html");
    let quote = "커피빈 카드 최초 등록시 무료음료 쿠폰 (3만원 이상 충전 시에만 지급)";
    let (anchors, _) = first_confirmation(&html, quote);
    // The window the older reader wrote: another version, another reading.
    let older = Some("w0:5만원,5만원,5천원".to_string());
    let again = assess_on(&html, &fact_row(quote, anchors, older, false));
    assert!(
        again.resolution.confirms(),
        "re-baselined on the same bytes: {:?}",
        again.resolution
    );
    assert!(
        matches!(
            again.resolution,
            Resolution::Exact(FactOutcome::Confirmed { adopted: true, .. })
        ),
        "the new reading is the baseline"
    );
}

/// On changed bytes a baseline of another reader is neither confirmed nor
/// refuted as changed: it is held for fresh extraction (never auto-adopt
/// the new window: that would confirm whatever the page says now).
#[test]
fn another_readers_baseline_on_a_changed_page_is_held_for_rereading() {
    let html = saved_real_page("247e7e73.html");
    let quote = "커피빈 카드 최초 등록시 무료음료 쿠폰 (3만원 이상 충전 시에만 지급)";
    let (anchors, _) = first_confirmation(&html, quote);
    let changed = html.replacen("</body>", "<!-- --><p>추가 안내</p></body>", 1);
    let again = assess_on(
        &changed,
        &fact_row(quote, anchors, Some("w0:5만원".to_string()), false),
    );
    assert_eq!(
        again.resolution,
        Resolution::Ambiguous(resilience::BASELINE_UNREADABLE.into())
    );
}

/// The current reader's own baseline behaves as before: unchanged values
/// confirm, changed values are `window_changed`.
#[test]
fn the_current_readers_baseline_confirms_and_refutes_as_before() {
    let html = saved_real_page("247e7e73.html");
    let quote = "커피빈 카드 최초 등록시 무료음료 쿠폰 (3만원 이상 충전 시에만 지급)";
    let (anchors, window) = first_confirmation(&html, quote);
    assert!(window.starts_with(super::WINDOW_READER));
    let same = assess_on(
        &html,
        &fact_row(quote, anchors.clone(), Some(window), false),
    );
    assert!(matches!(
        same.resolution,
        Resolution::Exact(FactOutcome::Confirmed { adopted: false, .. })
    ));
    let changed = html.replace("3만원 이상", "5만원 이상");
    let quote_changed = "커피빈 카드 최초 등록시 무료음료 쿠폰";
    let (anchors, window) = first_confirmation(&html, quote_changed);
    let refuted = assess_on(
        &changed,
        &fact_row(quote_changed, anchors, Some(window), false),
    );
    assert_eq!(refuted.resolution.label(), "window_changed");
    // A window stored before versions existed is read as this reader's.
    let legacy = window_baseline(Some("5만원,5천원"));
    assert_eq!(legacy, Ok(Some("5만원,5천원")));
}

/// The stored readings of every saved real page (value windows and section
/// digests of each audited fragment), hashed: what the baselines in
/// production were written with.
fn reader_output() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_pages");
    let index: Vec<crate::corporate_auto::snapshot::SnapshotEntry> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("index.json")).unwrap()).unwrap();
    let mut windows = Sha256::new();
    let mut sections = Sha256::new();
    for entry in index.iter().filter(|e| e.outcome == "stored") {
        let html = std::fs::read_to_string(dir.join(entry.file.as_deref().unwrap())).unwrap();
        let view = PageView::new(&html, &visible_text(&html));
        for fragment in &entry.fragments {
            windows.update(format!("{:?}\n", view.evidence.window_signature(fragment)).as_bytes());
            let captured = resilience::capture(&view, fragment);
            for identity in captured.iter().flat_map(|f| f.identities.iter()) {
                sections.update(identity.section.as_bytes());
                sections.update(b"\n");
            }
        }
    }
    format!(
        "{WINDOW_READER}{}|{}{}",
        hex(&windows.finalize()),
        resilience::SECTION_VERSION,
        hex(&sections.finalize())
    )
}

/// The reader's output on the saved real pages, pinned to its versions.
const READER_PIN: &str = "w1:0a2ab56d9f753329c86aeb0eb11cfabaac62b930c07f1730b86f6ab153919f2a|s4:193a0a7bc9c936c5044503f8b409dbd0ff43fb06935d42a02cefb838fb6b7fed";

/// Release gate (RC3). (1) A fact confirmed on a saved real page confirms
/// again on the same bytes with the baselines it stored. (2) The reader's
/// readings of those pages are pinned to [`WINDOW_READER`] and
/// [`resilience::SECTION_VERSION`]: a reader change that reads them
/// otherwise fails here until the version it changed is bumped (and the pin
/// re-taken), so stored baselines are never compared across readers.
#[test]
fn reader_versions_are_bumped_with_the_reader() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_pages");
    let index: Vec<crate::corporate_auto::snapshot::SnapshotEntry> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("index.json")).unwrap()).unwrap();
    let mut confirmed = 0;
    for entry in index.iter().filter(|e| e.outcome == "stored") {
        let html = std::fs::read_to_string(dir.join(entry.file.as_deref().unwrap())).unwrap();
        for fragment in &entry.fragments {
            let first = assess_on(&html, &fact_row(fragment, json!({}), None, true));
            let Resolution::Exact(FactOutcome::Confirmed {
                window_signature, ..
            }) = &first.resolution
            else {
                continue;
            };
            let anchors = serde_json::to_value(first.anchors.clone().unwrap()).unwrap();
            let again = assess_on(
                &html,
                &fact_row(
                    fragment,
                    anchors,
                    Some(stored_window(window_signature)),
                    false,
                ),
            );
            assert!(
                again.resolution.confirms(),
                "{}: {fragment:?} flipped on the same bytes: {:?}",
                entry.url,
                again.resolution
            );
            confirmed += 1;
        }
    }
    assert!(confirmed >= 20, "{confirmed} confirmations replayed");
    assert_eq!(
        reader_output(),
        READER_PIN,
        "the reader reads the saved pages otherwise: bump WINDOW_READER or SECTION_VERSION for what changed, then re-pin"
    );
}

/// F4: every reading and every source outcome has exactly one exit, and
/// the classes that only fresh reading can settle all go to rereading.
#[test]
fn every_hold_class_has_one_exit() {
    use resilience::{BASELINE_UNREADABLE, PAGE_SAYS_ENDED, SECTION_CHANGED, SECTION_DATE_OTHER};
    for resolution in [
        Resolution::Exact(FactOutcome::QuoteMissing),
        Resolution::Exact(FactOutcome::WindowChanged),
        Resolution::Exact(FactOutcome::FieldMismatch("age".into())),
        Resolution::Exact(FactOutcome::AdoptionExpired),
        Resolution::Ambiguous(SECTION_CHANGED.into()),
        Resolution::Ambiguous(SECTION_DATE_OTHER.into()),
        Resolution::Ambiguous(PAGE_SAYS_ENDED.into()),
        Resolution::Ambiguous(BASELINE_UNREADABLE.into()),
    ] {
        assert_eq!(fact_exit(&resolution), Exit::Reread, "{resolution:?}");
    }
    assert_eq!(
        fact_exit(&Resolution::Exact(FactOutcome::Confirmed {
            window_signature: String::new(),
            adopted: false
        })),
        Exit::Shown
    );
    for outcome in SOURCE_OUTCOMES {
        let expected = match *outcome {
            "confirmed" | "url_adopted" => Exit::Shown,
            "evidence_failed" | "adoption_expired" | "no_evidence" => Exit::Reread,
            "opted_out" | "redirected" | "retired_gone" => Exit::Terminal,
            _ => Exit::Wait,
        };
        assert_eq!(source_exit(outcome), expected, "{outcome}");
        // A waiting outcome is retried by this job: never a blocked class
        // without a schedule.
        if expected == Exit::Wait {
            assert_ne!(reverify::class_of(outcome), Class::Confirmed, "{outcome}");
        }
    }
    assert_eq!(
        source_exit("an outcome added later"),
        Exit::Terminal,
        "fail closed"
    );
}

/// The daily health row counts what RC4 and RC5 leave behind: a section
/// hold on an unchanged visible text, and an offer shown on a person's
/// timer past its deadline.
#[tokio::test]
async fn health_counts_section_holds_on_unchanged_text_and_expired_manual_timers() {
    let Some(pool) = db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DB_LOCK.lock().await;
    let site = site_with(long_nike_page());
    let url = serve(site.clone()).await;
    let f = fixture(&pool, &url).await;
    let metrics = super::super::health::collect(&pool).await.unwrap();
    let before = (
        metrics["resilience_24h"]["section_changed_same_text"]
            .as_i64()
            .unwrap(),
        metrics["monitors"]["human_corporate_overdue_shown"]
            .as_i64()
            .unwrap(),
    );
    sqlx::query(
        "INSERT INTO corporate_evidence_checks (candidate_id, program_id, mode, outcome, applied, details, checked_at)
         VALUES ($1, $2, 'live', 'confirmed', true, '{\"page_hash\": \"same\"}', now() - interval '2 hours'),
                ($1, $2, 'live', 'ambiguous', true,
                 jsonb_build_object('page_hash', 'same', 'facts', jsonb_build_array(jsonb_build_object('detail', $3::text))),
                 now() - interval '1 hour')",
    )
    .bind(f.candidate_id)
    .bind(f.program_id)
    .bind(resilience::SECTION_CHANGED)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE program_source_monitors SET verification_mode = 'human', review_due_at = now() - interval '1 day' WHERE program_id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE programs SET is_active = true, program_status = 'always_open', source_health = 'current' WHERE id = $1")
        .bind(f.program_id)
        .execute(&pool)
        .await
        .unwrap();
    let metrics = super::super::health::collect(&pool).await.unwrap();
    assert_eq!(
        metrics["resilience_24h"]["section_changed_same_text"]
            .as_i64()
            .unwrap(),
        before.0 + 1
    );
    assert_eq!(
        metrics["monitors"]["human_corporate_overdue_shown"]
            .as_i64()
            .unwrap(),
        before.1 + 1
    );
    assert!(
        metrics["reader_backlog"].is_object(),
        "the backlog is reported"
    );
    cleanup(&pool, &f).await;
}
