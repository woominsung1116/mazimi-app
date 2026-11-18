use super::*;
#[test]
fn probe_r4_bound() {
    let a = vec!["university_student", "student"];
    for (name, shown, quote) in [
        ("D0_control", "대학생 10% 할인", "대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다."),
        ("D1_first_month_dropped", "대학생 5,000원 할인", "대학생 첫 달 5,000원 할인"),
        ("D2_first_payment_only", "대학생 10% 할인", "대학생 첫 결제 시 10% 할인"),
        ("D3_threshold_split_hu", "대학생 10% 할인", "대학생 3만원 이상 구매 후 10% 할인"),
        ("D4_product_split_e", "대학생 10% 할인", "대학생 노트북 구매에 10% 할인"),
        ("D5_weekend_split_e", "대학생 10% 할인", "대학생 주말에 10% 할인"),
        ("D6_threshold_split_hayeo", "대학생 10% 할인", "대학생 5만원 이상 결제하여 10% 할인"),
        ("D7_negation_next_clause_e", "대학생 10% 할인", "대학생 10% 할인 혜택에 해당되지 않습니다"),
        ("D8_negation_next_clause_kkaji", "대학생 10% 할인", "대학생 10% 할인까지 받을 수 없습니다"),
        ("D9_time_window_kkaji", "대학생 10% 할인", "대학생 평일 오후 3시까지 10% 할인"),
        ("D10_membership_required", "대학생 10% 할인", "대학생 유료 멤버십 가입 시 10% 할인"),
        ("D11_extra_on_top", "대학생 10% 할인", "대학생 추가 10% 할인"),
        ("D12_pass_product", "대학생 10% 할인", "대학생 이용권 10% 할인"),
        ("D13_specific_plan", "대학생 10% 할인", "대학생 전용 요금제 10% 할인"),
        ("D14_before_meaning", "대학생 10% 할인", "대학생 결제 전 쿠폰 적용 시 10% 할인"),
        ("D15_audience_negated_by_allowed", "대학생 10% 할인", "대학생 대상 10% 할인 혜택 없음"),
        ("D16_first_only_once", "대학생 5,000원 할인", "대학생 최초 1회 5,000원 할인 쿠폰"),
        ("D17_kind_after_coupon", "대학생 5,000원 할인", "대학생 5,000원 할인 쿠폰 (3만원 이상 구매 시)"),
        ("D18_ui_hu_threshold", "대학생 10% 할인", "학생 인증 후 10% 할인"),
        ("D19_hu_after_payment_month", "대학생 매월 5,000원 할인", "대학생 가입 후 매월 5,000원 할인 쿠폰을 드립니다 첫 달만"),
        ("D20_unit_won_pct", "대학생 10% 할인", "대학생 10% 할인 적용 시 결제 금액 1만원 이상"),
    ] {
        println!("R4BOUND {name} => {:?}", displayed_values_bound(shown, &[quote], &a));
    }
}

#[test]
fn probe_r4_cost() {
    use crate::corporate_auto::bootstrap::audience_codes;
    let (mut n, mut ok, mut novalue) = (0, 0, 0);
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for item in crate::corporate_auto::evidence::audit().items.iter().filter(|i| i.verdict == "CORRECT") {
        let Ok(frags) = crate::corporate_auto::evidence::audit_fragments(item) else { continue };
        let quotes: Vec<&str> = frags.iter().map(String::as_str).collect();
        let aud = audience_codes(&format!("{} {}", item.title, item.quote.as_deref().unwrap_or("")));
        n += 1;
        if value_tokens(&item.title).is_empty() { novalue += 1; }
        match displayed_values_bound(&item.title, &quotes, &aud) {
            Ok(()) => ok += 1,
            Err(e) => { *reasons.entry(e.split(':').next().unwrap().to_string()).or_default() += 1; println!("COST_AUDIT_REFUSED {:?} quotes={:?} => {e}", item.title, quotes); }
        }
    }
    println!("COST_AUDIT correct_items={n} ok={ok} refused={} title_without_value={novalue} reasons={reasons:?}", n - ok);
    // real-page fixtures: title vs fragments
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real_pages/index.json")).unwrap()).unwrap();
    let entries = index.as_array().cloned().or_else(|| index.get("pages").and_then(|p| p.as_array().cloned())).unwrap_or_default();
    let (mut rn, mut rok, mut idn, mut idok) = (0, 0, 0, 0);
    for e in &entries {
        let title = e["title"].as_str().unwrap_or("");
        let frags: Vec<&str> = e["fragments"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        let aud = audience_codes(&format!("{title} {}", frags.join(" ")));
        rn += 1;
        match displayed_values_bound(title, &frags, &aud) { Ok(()) => rok += 1, Err(err) => println!("COST_REAL_REFUSED {title:?} {frags:?} => {err}") }
        for f in &frags {
            if value_tokens(f).is_empty() { continue; }
            idn += 1;
            match displayed_values_bound(f, &[f], &aud) { Ok(()) => idok += 1, Err(err) => println!("COST_IDENTITY_REFUSED {f:?} => {err}") }
        }
    }
    println!("COST_REAL titles={rn} ok={rok} refused={} | identity_fragments_with_value={idn} ok={idok} refused={}", rn - rok, idn - idok);
    // corpus quotes (distinct, from the differential samples), shown as themselves
    if let Ok(path) = std::env::var("COST_SAMPLES") {
        let mut seen = std::collections::BTreeSet::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let s: serde_json::Value = serde_json::from_str(line).unwrap();
            if let Some(q) = s["quote"].as_str() { for f in crate::corporate_auto::evidence::split_fragments(q) { seen.insert(f.to_string()); } }
        }
        let (mut cn, mut cok) = (0, 0);
        for q in &seen {
            if value_tokens(q).is_empty() { continue; }
            cn += 1;
            let aud = audience_codes(q);
            match displayed_values_bound(q, &[q], &aud) { Ok(()) => cok += 1, Err(err) => println!("COST_CORPUS_REFUSED {q:?} => {err}") }
        }
        println!("COST_CORPUS distinct_quotes_with_value={cn} ok={cok} refused={}", cn - cok);
    }
}
