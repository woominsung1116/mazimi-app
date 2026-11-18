//! Reviewer's hand-written adversarial pages (round 4). Not part of the
//! corpus; no code shared with the corpus generator.
use super::*;

fn page(body: &str) -> String {
    format!("<html><head><title>t</title></head><body><nav><a href=\"/\">홈</a></nav><section><h2>학생 혜택 안내</h2>{body}</section><footer>고객센터 1588-0000</footer></body></html>")
}

/// Production flow: a seeded fact (no anchors, adoption open) is first
/// resolved on `before`; on confirmation its anchors are captured. Then it is
/// resolved on `after` with the adopted baseline.
fn run(before: &str, after: &str, quote: &str) -> (String, Resolution) {
    let bv = super::view(before);
    let empty = Anchors::default();
    let first = resolve(
        &bv,
        &StoredFact {
            quote,
            baseline: None,
            ages: Ages {
                min: None,
                max: None,
            },
            adoption_open: true,
            anchors: &empty,
        },
    );
    let window = match &first {
        Resolution::Exact(FactOutcome::Confirmed {
            window_signature, ..
        }) => window_signature.clone(),
        Resolution::Fuzzy { window_signature } => window_signature.clone(),
        Resolution::Relocated {
            window_signature, ..
        } => window_signature.clone(),
        other => return (format!("SETUP-NOT-CONFIRMED {:?}", other), other.clone()),
    };
    let anchors = Anchors {
        fingerprint: capture(&bv, quote),
        ..Anchors::default()
    };
    let ids = anchors
        .fingerprint
        .as_ref()
        .map(|f| f.identities.clone())
        .unwrap_or_default();
    let av = super::view(after);
    let r = resolve(
        &av,
        &StoredFact {
            quote,
            baseline: Some(&window),
            ages: Ages {
                min: None,
                max: None,
            },
            adoption_open: false,
            anchors: &anchors,
        },
    );
    (format!("ids={:?}", ids), r)
}

/// First sight on a page (new fact, no anchors, adoption open).
fn first_sight(html: &str, quote: &str) -> Resolution {
    let v = super::view(html);
    let empty = Anchors::default();
    resolve(
        &v,
        &StoredFact {
            quote,
            baseline: None,
            ages: Ages {
                min: None,
                max: None,
            },
            adoption_open: true,
            anchors: &empty,
        },
    )
}

fn verdict(truth: &str, r: &Resolution) -> &'static str {
    match (truth, r) {
        ("same", r) if r.confirms() => "ok-confirm",
        ("same", _) => "false-hide",
        (_, r) if r.confirms() => "FALSE_CONFIRM",
        (t, Resolution::Proposed(p))
            if t.starts_with("changed:") && p.new_signature == t["changed:".len()..] =>
        {
            "ok-update"
        }
        (_, Resolution::Proposed(_)) => "WRONG_UPDATE",
        _ => "ok-hidden",
    }
}

#[test]
fn reviewer_adversarial_pages() {
    let td = |rows: &[(&str, &str)]| {
        format!(
            "<table><tbody>{}</tbody></table>",
            rows.iter()
                .map(|(l, v)| format!("<tr><td>{l}</td><td>{v}</td></tr>"))
                .collect::<String>()
        )
    };
    let divs = |rows: &[(&str, &str)]| {
        rows.iter().map(|(l, v)| format!("<div class=\"card\"><div class=\"name\">{l}</div><div class=\"desc\">{v}</div></div>")).collect::<String>()
    };
    let spans = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(l, v)| format!("<p class=\"row\"><span>{l}</span><span>{v}</span></p>"))
            .collect::<String>()
    };
    let nested = |rows: &[(&str, &str)]| {
        format!(
            "<ul>{}</ul>",
            rows.iter()
                .map(|(l, v)| format!("<li>{l}<ul><li>{v}</li></ul></li>"))
                .collect::<String>()
        )
    };
    let imgs = |rows: &[(&str, &str)]| {
        format!(
            "<ul>{}</ul>",
            rows.iter()
                .map(|(l, v)| format!("<li><img src=\"x.png\" alt=\"{l}\"><span>{v}</span></li>"))
                .collect::<String>()
        )
    };
    let aria = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(l, v)| format!("<div role=\"group\" aria-label=\"{l}\"><p>{v}</p></div>"))
            .collect::<String>()
    };
    let deep = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(l, v)| format!("<h3>{l}</h3><h4>혜택</h4><h5>상세</h5><p>{v}</p>"))
            .collect::<String>()
    };
    let q = "월 3,000원 할인";
    let same3 = [("1인", q), ("2인", q), ("3인", q)];
    let del1 = [("2인", q), ("3인", q)];
    let del1_fmt = [("2인", "월 3000원 할인"), ("3인", "월 3000원 할인")];
    let del1_chg = [("2인", "월 5,000원 할인"), ("3인", q)];
    let mut cases: Vec<(String, String, String, String, String)> = Vec::new();
    type Layout<'a> = (&'a str, &'a dyn Fn(&[(&str, &str)]) -> String);
    let layouts: Vec<Layout> = vec![
        ("td-only table", &td),
        ("div/div cards", &divs),
        ("span/span rows", &spans),
        ("nested li", &nested),
        ("img alt label", &imgs),
        ("aria-label group", &aria),
        ("deep headings", &deep),
    ];
    for (name, f) in &layouts {
        cases.push((
            format!("{name}: identical tiers, delete 1인 (exact)"),
            page(&f(&same3)),
            page(&f(&del1)),
            q.into(),
            "gone".into(),
        ));
        cases.push((
            format!("{name}: identical tiers, delete 1인 + 3,000->3000 reformat"),
            page(&f(&same3)),
            page(&f(&del1_fmt)),
            q.into(),
            "gone".into(),
        ));
        cases.push((
            format!("{name}: identical tiers, delete 1인, 2인 -> 5,000"),
            page(&f(&same3)),
            page(&f(&del1_chg)),
            q.into(),
            "gone".into(),
        ));
        // distinct wording per tier so keys differ; swap labels
        let a = [("1인", q), ("2인", "매월 5,000원 할인")];
        let b = [("1인", "매월 5,000원 할인"), ("2인", q)];
        cases.push((
            format!("{name}: distinct wording, labels swapped"),
            page(&f(&a)),
            page(&f(&b)),
            q.into(),
            "gone".into(),
        ));
    }
    // Two sections with the same tier names, section titles not headings.
    let sec = |title: &str, v: &str| {
        format!(
            "<div class=\"sec\"><div class=\"sec-tit\">{title}</div><h4>1인</h4><p>{v}</p></div>"
        )
    };
    cases.push((
        "same tier names in two sections, student section dropped".into(),
        page(&format!(
            "{}{}",
            sec("학생 요금제", q),
            sec("가족 요금제", q)
        )),
        page(&sec("가족 요금제", q)),
        q.into(),
        "gone".into(),
    ));
    // Strike-through of the old value next to the new one.
    cases.push(("<del> old value, new value in <strong>".into(),
        page("<div class=\"price\"><span>학생 월 4,900원</span></div>"),
        page("<div class=\"price\"><span><del>학생 월 4,900원</del></span> <strong>학생 월 5,900원</strong></div>"), "학생 월 4,900원".into(), "gone".into()));
    cases.push(("<s> old quote kept, new value in sibling p".into(),
        page("<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div>"),
        page("<div class=\"c\"><h3>1인</h3><p><s>월 3,000원 할인</s></p><div>11월부터 월 2,000원 할인</div></div>"), q.into(), "gone".into()));
    // End dates the regex misses.
    for (label, note) in [
        (
            "hyphen date range (past)",
            "이벤트 기간: 2026.03.01 - 2026.06.30",
        ),
        ("date + time before 까지", "2026.06.30 23:59까지"),
        ("month-only 까지", "2026년 6월까지 진행"),
        ("no-year ~MM.DD", "~06.30"),
        ("'기간이 지났습니다'", "이벤트 기간이 지났습니다."),
        ("'END' badge", "END"),
        ("'지난 이벤트' badge", "지난 이벤트"),
        ("'오픈 예정'", "오픈 예정"),
        ("'Coming soon'", "Coming soon"),
        ("'This offer has expired'", "This offer has expired."),
        ("'Offer ended'", "Offer ended"),
        (
            "condition added '첫 결제 시 1회'",
            "※ 첫 결제 시 1회에 한함",
        ),
        ("'신규 가입자 한정' condition", "신규 가입자 한정"),
        ("'재학생만 해당되며 휴학생은 X'", "휴학생 X"),
    ] {
        cases.push((
            format!("sibling note: {label}"),
            page("<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div>"),
            page(&format!(
                "<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p><p>{note}</p></div>"
            )),
            q.into(),
            "gone".into(),
        ));
    }
    // Banner far from the quote in a large section.
    let filler = "<p>학생 회원이라면 누구나 다양한 제휴처에서 할인과 적립 혜택을 받을 수 있습니다. 자세한 내용은 아래 안내를 확인해 주세요.</p>".repeat(4);
    cases.push(("section banner '종료된 이벤트입니다' far from quote".into(),
        page(&format!("<div class=\"evt\">{filler}<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div></div>")),
        page(&format!("<div class=\"evt\"><div class=\"banner\">본 이벤트는 종료되었습니다.</div>{filler}<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div></div>")), q.into(), "gone".into()));
    // Modal: quote moves into a hidden modal, new value visible.
    cases.push(("quote moved into display:none modal, new value visible".into(),
        page("<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div>"),
        page("<div class=\"c\"><h3>1인</h3><p>월 4,000원 할인</p></div><div class=\"modal\" style=\"display: none\"><h3>1인</h3><p>월 3,000원 할인</p></div>"), q.into(), "changed:4000원".into()));
    // FAQ accordion repeating the quote with a negative answer.
    cases.push(("FAQ: offer card removed, quote only in FAQ question".into(),
        page("<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div><details><summary>졸업생도 받을 수 있나요?</summary><p>재학생만 가능합니다.</p></details>"),
        page("<details><summary>월 3,000원 할인</summary><p>아니요. 현재는 제공되지 않습니다.</p></details>"), q.into(), "gone".into()));
    // A/B: the variant with another value under the same heading.
    cases.push((
        "A/B variant: same place, other value".into(),
        page("<div class=\"c\"><h3>1인</h3><p>월 3,000원 할인</p></div>"),
        page("<div class=\"c\"><h3>1인</h3><p>월 4,000원 할인</p></div>"),
        q.into(),
        "changed:4000원".into(),
    ));
    // 만원 / 원, ranges, $.
    cases.push((
        "만원 spacing only".into(),
        page("<p>월 1만원 할인</p>"),
        page("<p>월 1만 원 할인</p>"),
        "월 1만원 할인".into(),
        "same".into(),
    ));
    cases.push((
        "range upper bound changed".into(),
        page("<h3>1인</h3><p>3,000원~5,000원 할인</p>"),
        page("<h3>1인</h3><p>3,000원~7,000원 할인</p>"),
        "3,000원~5,000원 할인".into(),
        "gone".into(),
    ));
    cases.push((
        "$5 -> $15 (digit-prefixed quote)".into(),
        page("<h3>Student</h3><p>$5 off for students.</p>"),
        page("<h3>Student</h3><p>$15 off for students.</p>"),
        "$5 off for students".into(),
        "gone".into(),
    ));
    cases.push((
        "5% -> 15% (digit-prefixed quote), formatting change".into(),
        page("<h3>1단계</h3><p>5% 적립</p>"),
        page("<h3>1단계</h3><p>15 % 적립!</p>"),
        "5% 적립".into(),
        "gone".into(),
    ));
    cases.push((
        "relative date '이번 달 말까지' added".into(),
        page("<h3>1인</h3><p>월 3,000원 할인</p>"),
        page("<h3>1인</h3><p>월 3,000원 할인</p><p>이번 달 말까지</p>"),
        q.into(),
        "same".into(),
    ));
    cases.push(("갱신 notice: changes next month".into(), page("<h3>1인</h3><p>월 3,000원 할인</p>"), page("<h3>1인</h3><p>월 3,000원 할인</p><p>2027년 1월 1일부터 월 2,000원 할인으로 변경됩니다.</p>"), q.into(), "same".into()));

    let mut bad = 0;
    for (name, before, after, quote, truth) in &cases {
        let (ids, r) = run(before, after, quote);
        let v = verdict(truth, &r);
        if v == "FALSE_CONFIRM" || v == "WRONG_UPDATE" {
            bad += 1;
        }
        eprintln!("[{v:>13}] {name}\n      -> {}  {ids}", r.label());
    }
    // First sight (new fact, no identities): digit-prefixed quotes.
    for (name, html, quote) in [
        (
            "first sight: '5% 적립' on a '15 % 적립' page",
            page("<p>15 % 적립!</p>"),
            "5% 적립",
        ),
        (
            "first sight: '$5 off for students' on '$15 off' page",
            page("<p>$15 off for students.</p>"),
            "$5 off for students",
        ),
        (
            "first sight: '3,000원 할인 쿠폰' on '13,000원 할인 쿠폰!' page",
            page("<li>13,000원 할인 쿠폰!</li>"),
            "3,000원 할인 쿠폰",
        ),
    ] {
        let r = first_sight(&html, quote);
        let v = verdict("gone", &r);
        if v == "FALSE_CONFIRM" {
            bad += 1;
        }
        eprintln!("[{v:>13}] {name}\n      -> {} {:?}", r.label(), r);
    }
    eprintln!("bad = {bad}");
    // Gate added on import (fixtures and expected outcomes untouched).
    assert_eq!(
        bad, 0,
        "false confirms or wrong updates on the reviewer's pages"
    );
}

#[test]
fn reviewer_dedup_enables_wrong_update() {
    let q = "월 3,000원 할인";
    for (name, before, after) in [
        (
            "td-only 2 identical tiers; 1인 row removed, 2인 -> 5,000",
            page(&format!(
                "<table><tr><td>1인</td><td>{q}</td></tr><tr><td>2인</td><td>{q}</td></tr></table>"
            )),
            page("<table><tr><td>2인</td><td>월 5,000원 할인</td></tr></table>"),
        ),
        (
            "div cards 2 identical tiers; 1인 card removed, 2인 -> 5,000",
            page(&format!(
                "<div><div>1인</div><div>{q}</div></div><div><div>2인</div><div>{q}</div></div>"
            )),
            page("<div><div>2인</div><div>월 5,000원 할인</div></div>"),
        ),
        (
            "h3-labelled control (labels detected)",
            page(&format!("<h3>1인</h3><p>{q}</p><h3>2인</h3><p>{q}</p>")),
            page("<h3>2인</h3><p>월 5,000원 할인</p>"),
        ),
    ] {
        let (ids, r) = run(&before, &after, q);
        let v = verdict("gone", &r);
        eprintln!("[{:>13}] {name}\n      -> {:?}  {ids}", v, r);
        // Gate added on import.
        assert!(v != "WRONG_UPDATE" && v != "FALSE_CONFIRM", "{name}: {v}");
    }
}
