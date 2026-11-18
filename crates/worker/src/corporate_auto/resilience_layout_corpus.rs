//! Layout-only corpus: the same page text in 12 DOM shapes (headings and
//! paragraphs, tables, a definition list, cards, list items, deep wrappers,
//! ARIA headings, React text-split markers, pretty-printed markup, semantic
//! tags, flex rows). Every fact seeded on one shape must still confirm on
//! every other shape: text unchanged, structure redesigned. At least 95%
//! must confirm, and none may be proposed as an update.

use std::collections::BTreeMap;

use crate::corporate_auto::evidence::{visible_text, Ages, FactOutcome};
use crate::corporate_auto::resilience::{
    capture, resolve, Anchors, PageView, Resolution, StoredFact,
};

struct Content {
    heading: &'static str,
    tiers: &'static [(&'static str, &'static str)],
    note: &'static str,
}

const CONTENTS: &[Content] = &[
    Content {
        heading: "멤버십 등급별 혜택",
        tiers: &[
            ("VIP", "결제 금액의 5% 적립"),
            ("골드", "결제 금액의 3% 적립"),
            ("일반", "결제 금액의 1% 적립"),
        ],
        note: "※ 적립금은 다음 달 15일에 지급됩니다.",
    },
    Content {
        heading: "학생 할인",
        tiers: &[
            ("학부생", "학생 인증 시 10% 할인"),
            ("대학원생", "학생 인증 시 15% 할인"),
        ],
        note: "인증은 매 학기 갱신됩니다.",
    },
    Content {
        heading: "제휴 카드 혜택",
        tiers: &[
            ("A카드", "월 3,000원 할인"),
            ("B카드", "월 5,000원 할인"),
            ("C카드", "월 7,000원 할인"),
        ],
        note: "* 전월 실적 30만원 이상 시",
    },
    Content {
        heading: "Student offers",
        tiers: &[
            ("Undergraduate", "10% off for students"),
            ("Graduate", "15% off for students"),
        ],
        note: "Verification renews each term.",
    },
];

type Shape = fn(&Content) -> String;

fn each(c: &Content, row: impl Fn(&str, &str) -> String) -> String {
    c.tiers.iter().map(|(l, s)| row(l, s)).collect()
}

fn heading_paragraphs(c: &Content) -> String {
    format!(
        "<section><h2>{}</h2>{}<p class=\"note\">{}</p></section>",
        c.heading,
        each(c, |l, s| format!("<h3>{l}</h3><p>{s}</p>")),
        c.note
    )
}

fn row_table(c: &Content) -> String {
    format!(
        "<h2>{}</h2><table><tbody>{}</tbody></table><p>{}</p>",
        c.heading,
        each(c, |l, s| format!("<tr><th>{l}</th><td>{s}</td></tr>")),
        c.note
    )
}

fn td_table(c: &Content) -> String {
    format!(
        "<h2 class=\"tbl-tit\">{}</h2><table class=\"benefit\">{}</table><div class=\"desc\">{}</div>",
        c.heading,
        each(c, |l, s| format!("<tr><td class=\"name\">{l}</td><td>{s}</td></tr>")),
        c.note
    )
}

fn definition_list(c: &Content) -> String {
    format!(
        "<h2>{}</h2><dl>{}</dl><p>{}</p>",
        c.heading,
        each(c, |l, s| format!("<dt>{l}</dt><dd>{s}</dd>")),
        c.note
    )
}

fn cards(c: &Content) -> String {
    format!(
        "<div class=\"section\"><h2 class=\"title\">{}</h2><div class=\"cards\">{}</div><div class=\"foot\"><p>{}</p></div></div>",
        c.heading,
        each(c, |l, s| format!(
            "<div class=\"card\"><div class=\"card-head\"><strong>{l}</strong></div><div class=\"card-body\"><p>{s}</p></div></div>"
        )),
        c.note
    )
}

fn list_items(c: &Content) -> String {
    format!(
        "<h2>{}</h2><ul class=\"tiers\">{}</ul><p>{}</p>",
        c.heading,
        each(c, |l, s| format!("<li><strong>{l}</strong> {s}</li>")),
        c.note
    )
}

fn deep_wrappers(c: &Content) -> String {
    let open = "<div class=\"w\">".repeat(9);
    let close = "</div>".repeat(9);
    format!("{open}{}{close}", heading_paragraphs(c))
}

fn aria_headings(c: &Content) -> String {
    format!(
        "<div class=\"box\"><div role=\"heading\" aria-level=\"2\">{}</div>{}<div class=\"note\">{}</div></div>",
        c.heading,
        each(c, |l, s| format!(
            "<div class=\"tier\"><div role=\"heading\" aria-level=\"3\">{l}</div><div>{s}</div></div>"
        )),
        c.note
    )
}

/// React splits text nodes with empty comments (`5<!-- -->% 적립`).
fn react_split(c: &Content) -> String {
    format!(
        "<section><h2>{}</h2>{}<p class=\"note\">{}</p></section>",
        c.heading,
        each(c, |l, s| {
            let split = s
                .replacen(' ', " <!-- -->", 1)
                .replacen('%', "<!-- -->%", 1);
            format!("<h3>{l}</h3><p>{split}</p>")
        }),
        c.note
    )
}

fn pretty_printed(c: &Content) -> String {
    format!(
        "\n  <h2>\n    {}\n  </h2>\n  <table>\n    <tbody>\n{}    </tbody>\n  </table>\n  <p>\n    {}\n  </p>\n",
        c.heading,
        each(c, |l, s| format!(
            "      <tr>\n        <th>\n          {l}\n        </th>\n        <td>\n          {s}\n        </td>\n      </tr>\n"
        )),
        c.note
    )
}

fn semantic(c: &Content) -> String {
    format!(
        "<article><header><h2>{}</h2></header>{}<footer><small>{}</small></footer></article>",
        c.heading,
        each(c, |l, s| format!(
            "<section><h4>{l}</h4><div><span>{s}</span></div></section>"
        )),
        c.note
    )
}

fn flex_rows(c: &Content) -> String {
    format!(
        "<div class=\"grid\"><h2>{}</h2>{}<p class=\"caption\">{}</p></div>",
        c.heading,
        each(c, |l, s| {
            format!(
            "<div class=\"row\"><div class=\"col label\">{l}</div><div class=\"col value\">{s}</div></div>"
        )
        }),
        c.note
    )
}

const SHAPES: &[(&str, Shape)] = &[
    ("heading_paragraphs", heading_paragraphs),
    ("row_table", row_table),
    ("td_table", td_table),
    ("definition_list", definition_list),
    ("cards", cards),
    ("list_items", list_items),
    ("deep_wrappers", deep_wrappers),
    ("aria_headings", aria_headings),
    ("react_split", react_split),
    ("pretty_printed", pretty_printed),
    ("semantic", semantic),
    ("flex_rows", flex_rows),
];

fn page(main: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><title>혜택 안내</title></head>\
         <body><header class=\"gnb\"><a href=\"/\">홈</a> <a href=\"/benefit\">혜택</a></header>\
         <main>{main}</main><footer><p>고객센터 1588-1234</p></footer></body></html>"
    )
}

fn view(html: &str) -> PageView {
    PageView::new(html, &visible_text(html))
}

const NO_AGES: Ages = Ages {
    min: None,
    max: None,
};

/// Seeds `quote` on `before` as the evidence job does (adopt, capture,
/// confirm again with the anchors) and resolves it on `after`.
fn replay(before: &str, after: &str, quote: &str) -> Result<Resolution, String> {
    let page = view(before);
    let empty = Anchors::default();
    let fact = |baseline, anchors, open| StoredFact {
        quote,
        baseline,
        ages: NO_AGES,
        adoption_open: open,
        anchors,
    };
    let window = match resolve(&page, &fact(None, &empty, true)) {
        Resolution::Exact(FactOutcome::Confirmed {
            window_signature,
            adopted: true,
        }) => window_signature,
        other => return Err(format!("seed not adopted: {other:?}")),
    };
    let anchors = Anchors {
        fingerprint: capture(&page, quote),
        ..Anchors::default()
    };
    let again = resolve(&page, &fact(Some(&window), &anchors, false));
    if !again.confirms() {
        return Err(format!("second confirm failed: {again:?}"));
    }
    Ok(resolve(&view(after), &fact(Some(&window), &anchors, false)))
}

#[test]
fn the_same_text_in_another_layout_still_confirms() {
    let mut total = 0usize;
    let mut confirmed = 0usize;
    let mut misses: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for content in CONTENTS {
        for (before_name, before) in SHAPES {
            for (after_name, after) in SHAPES {
                let (before_html, after_html) = (page(&before(content)), page(&after(content)));
                for (_, quote) in content.tiers {
                    total += 1;
                    match replay(&before_html, &after_html, quote) {
                        Ok(resolution) if resolution.confirms() => confirmed += 1,
                        Ok(Resolution::Proposed(proposal)) => {
                            panic!("{before_name} -> {after_name} {quote}: proposed {proposal:?}")
                        }
                        other => {
                            *misses.entry((before_name, after_name)).or_default() += 1;
                            if misses.values().sum::<usize>() <= 5 {
                                eprintln!(
                                    "layout miss {before_name} -> {after_name} {quote}: {other:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let rate = confirmed as f64 / total as f64;
    eprintln!(
        "layout corpus: {confirmed} of {total} confirmed ({:.1}%), {} shapes, misses by pair: {misses:?}",
        rate * 100.0,
        SHAPES.len()
    );
    assert!(
        rate >= 0.95,
        "only {:.1}% confirmed: {misses:?}",
        rate * 100.0
    );
}
