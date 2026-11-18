//! Generated adversarial corpus for the resolver: thousands of rendered
//! page pairs (before, after) over layouts, tier labels, sentence templates,
//! value configurations and edits, each with its ground truth.
//!
//! Independence from the implementation: the ground truth uses its own
//! literal expected values (never `value_signature`), and endings and
//! conditions are this generator's own phrases taken from real Korean and
//! English offer pages, many of them on no list in the implementation. The
//! resolver must never confirm a fact the after-page no longer states and
//! never propose a value the page does not give this offer.
//!
//! Deterministic: every combination of layout, label set, template, value
//! configuration and edit is enumerated; the target tier, the phrase and
//! long-section padding are chosen by a fixed xorshift seed.

use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layout {
    HeadingParagraph,
    RowTable,
    ColumnTable,
    DefinitionList,
    Cards,
    ListItems,
    Tabs,
    // Layouts that give no th/dt/heading per tier.
    TdOnlyTable,
    PlainDivs,
    SpanRows,
    NestedList,
    ImageAlt,
    AriaGroups,
    DeepHeadings,
}

const LAYOUTS: [Layout; 14] = [
    Layout::HeadingParagraph,
    Layout::RowTable,
    Layout::ColumnTable,
    Layout::DefinitionList,
    Layout::Cards,
    Layout::ListItems,
    Layout::Tabs,
    Layout::TdOnlyTable,
    Layout::PlainDivs,
    Layout::SpanRows,
    Layout::NestedList,
    Layout::ImageAlt,
    Layout::AriaGroups,
    Layout::DeepHeadings,
];

const LABEL_SETS: [[&str; 3]; 7] = [
    ["1인", "2인", "3인"],
    ["Lv.1", "Lv.2", "Lv.3"],
    ["1단계", "2단계", "3단계"],
    ["1개월 이용권", "3개월 이용권", "12개월 이용권"],
    ["코드 01", "코드 02", "코드 03"],
    ["Basic", "Pro", "Max"],
    ["베이직", "스탠다드", "프리미엄"],
];

/// A sentence template with literal values and their literal expected
/// readings (written out by hand, not computed by the implementation).
struct Template {
    text: &'static str,
    /// Values as shown, and as the page means them (`3,000` → `3000원`).
    values: [(&'static str, &'static str); 3],
    /// A value no tier has.
    fresh: (&'static str, &'static str),
    /// The same sentence for another audience.
    audience: &'static str,
    /// A pure formatting change of the value (same meaning).
    reformat: fn(&str) -> String,
    english: bool,
}

fn drop_commas(value: &str) -> String {
    value.replace(',', "")
}

fn full_width(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(c as u32 + 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

const TEMPLATES: [Template; 4] = [
    Template {
        text: "월 {v}원 할인",
        values: [
            ("3,000", "3000원"),
            ("5,000", "5000원"),
            ("7,000", "7000원"),
        ],
        fresh: ("4,500", "4500원"),
        audience: "일반 고객 월 {v}원 할인",
        reformat: drop_commas,
        english: false,
    },
    Template {
        text: "구매 금액의 {v}% 적립",
        values: [("3", "3%"), ("5", "5%"), ("7", "7%")],
        fresh: ("4", "4%"),
        audience: "일반 회원 구매 금액의 {v}% 적립",
        reformat: full_width,
        english: false,
    },
    Template {
        text: "학생 인증 시 {v}% 할인",
        values: [("10", "10%"), ("15", "15%"), ("20", "20%")],
        fresh: ("12", "12%"),
        audience: "교직원 인증 시 {v}% 할인",
        reformat: full_width,
        english: false,
    },
    Template {
        text: "{v}% off for students",
        values: [("10", "10%"), ("15", "15%"), ("20", "20%")],
        fresh: ("12", "12%"),
        audience: "{v}% off for everyone",
        reformat: full_width,
        english: true,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Edit {
    Unchanged,
    RenameClasses,
    Reorder,
    MoveToNewPage,
    Reformat,
    DeleteTier,
    ChangeValue,
    ChangeAudience,
    PhraseInStatement,
    PhraseBesideAfter,
    PhraseBesideBefore,
    SectionBanner,
    SwapLabels,
    HiddenOldCopy,
    PastEndDate,
    ReformatAndDelete,
    ReformatAndChange,
    ReformatAndSwap,
}

const EDITS: [Edit; 18] = [
    Edit::Unchanged,
    Edit::RenameClasses,
    Edit::Reorder,
    Edit::MoveToNewPage,
    Edit::Reformat,
    Edit::DeleteTier,
    Edit::ChangeValue,
    Edit::ChangeAudience,
    Edit::PhraseInStatement,
    Edit::PhraseBesideAfter,
    Edit::PhraseBesideBefore,
    Edit::SectionBanner,
    Edit::SwapLabels,
    Edit::HiddenOldCopy,
    Edit::PastEndDate,
    Edit::ReformatAndDelete,
    Edit::ReformatAndChange,
    Edit::ReformatAndSwap,
];

/// Endings, withdrawals and restrictions as real offer pages write them.
/// Deliberately not the implementation's lists; several use wording no
/// list has ("막을 내렸습니다", "빠집니다", "곧 찾아옵니다", "withdrawn").
pub(super) const KO_PHRASES: &[&str] = &[
    "이벤트가 종료되었습니다",
    "본 혜택은 마감되었습니다",
    "행사가 막을 내렸습니다",
    "지금은 신청하실 수 없어요",
    "판매가 중단되었습니다",
    "선착순 마감",
    "혜택 제공이 끝났어요",
    "곧 찾아올 혜택입니다",
    "온라인 결제 시에만 적용",
    "신입생은 대상에서 빠집니다",
    "※ 1인 1회 한정",
    "기간 만료",
    "재고 소진 시 조기 종료",
    "현재 일시 품절",
];
pub(super) const EN_PHRASES: &[&str] = &[
    "This deal has ended",
    "Promotion closed",
    "Offer expired on June 30",
    "Coming soon",
    "Not valid for graduate students",
    "No longer offered",
    "This offer has been withdrawn",
    "Unavailable in your region",
    "Limited to new members",
    "Paused until further notice",
];
const KO_PAST: &str = "(이벤트 기간: 2021.03.01 - 2021.06.30)";
const EN_PAST: &str = "(valid until 2021-06-30)";

#[derive(Clone, Debug, PartialEq)]
enum Truth {
    /// The after-page states the fact unchanged: confirming is right,
    /// hiding is a false hide (allowed, counted).
    Same,
    /// The after-page no longer states it: confirming or proposing is wrong.
    Gone,
    /// The after-page gives this offer this new value (literal reading).
    Changed(&'static str),
}

#[derive(Clone)]
struct Tier {
    label: &'static str,
    text: String,
    /// Inside the statement's own element, after its sentence.
    inside: Option<String>,
    /// A separate element right after / before the statement.
    after: Option<String>,
    before: Option<String>,
}

struct Classes {
    block: &'static str,
    title: &'static str,
    text: &'static str,
}

const CLASSES: [Classes; 2] = [
    Classes {
        block: "plan-card",
        title: "plan-card__name",
        text: "plan-card__desc",
    },
    Classes {
        block: "sc-x81f",
        title: "sc-x81f-1 Title_t__9k",
        text: "sc-x81f-2 Body_b__2m",
    },
];

fn statement(tier: &Tier) -> String {
    match &tier.inside {
        Some(extra) => format!("{} {extra}", tier.text),
        None => tier.text.clone(),
    }
}

/// The statement element plus its beside-notes, as `tag` elements.
fn with_notes(tier: &Tier, tag: &str, class: &str) -> String {
    let note = |text: &Option<String>| {
        text.as_ref()
            .map(|t| format!("<{tag} class=\"note\">{t}</{tag}>"))
            .unwrap_or_default()
    };
    format!(
        "{}<{tag} class=\"{class}\">{}</{tag}>{}",
        note(&tier.before),
        statement(tier),
        note(&tier.after)
    )
}

const FILLER: &str = "학생 회원이라면 누구나 다양한 제휴처에서 할인과 적립 혜택을 받을 수 있습니다. 혜택 내용은 제휴사 사정에 따라 달라질 수 있으니 이용 전 꼭 확인해 주세요.";

#[allow(clippy::too_many_arguments)]
fn render(
    layout: Layout,
    tiers: &[Tier],
    classes: &Classes,
    active: usize,
    extra: &str,
    banner: Option<&str>,
    wrap: bool,
    long: bool,
) -> String {
    let body = match layout {
        Layout::HeadingParagraph => tiers
            .iter()
            .map(|t| {
                format!(
                    "<div class=\"{}\"><h3>{}</h3>{}</div>",
                    classes.block,
                    t.label,
                    with_notes(t, "p", classes.text)
                )
            })
            .collect::<String>(),
        Layout::RowTable => format!(
            "<table class=\"{}\">{}</table>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!(
                    "<tr><th>{}</th>{}</tr>",
                    t.label,
                    with_notes(t, "td", classes.text)
                ))
                .collect::<String>()
        ),
        Layout::ColumnTable => format!(
            "<table class=\"{}\"><tr><th>구분</th>{}</tr><tr><th>혜택</th>{}</tr></table>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!("<th>{}</th>", t.label))
                .collect::<String>(),
            tiers
                .iter()
                .map(|t| format!(
                    "<td class=\"{}\">{}</td>",
                    classes.text,
                    [t.before.clone(), Some(statement(t)), t.after.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" ")
                ))
                .collect::<String>()
        ),
        Layout::DefinitionList => format!(
            "<dl class=\"{}\">{}</dl>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!("<dt>{}</dt>{}", t.label, with_notes(t, "dd", classes.text)))
                .collect::<String>()
        ),
        Layout::Cards => tiers
            .iter()
            .map(|t| {
                format!(
                    "<div class=\"{}\"><div class=\"{}\">{}</div>{}</div>",
                    classes.block,
                    classes.title,
                    t.label,
                    with_notes(t, "p", classes.text)
                )
            })
            .collect::<String>(),
        Layout::ListItems => format!(
            "<ul class=\"{}\">{}</ul>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!(
                    "<li><strong>{}</strong> {}</li>",
                    t.label,
                    with_notes(t, "span", classes.text)
                ))
                .collect::<String>()
        ),
        Layout::Tabs => format!(
            "<div class=\"tabs\">{}</div>",
            tiers
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    format!(
                        "<div class=\"tab-pane{}\"><h3>{}</h3>{}</div>",
                        if i == active { " active" } else { "" },
                        t.label,
                        with_notes(t, "p", classes.text)
                    )
                })
                .collect::<String>()
        ),
        Layout::TdOnlyTable => format!(
            "<table class=\"{}\"><tbody>{}</tbody></table>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!(
                    "<tr><td>{}</td>{}</tr>",
                    t.label,
                    with_notes(t, "td", classes.text)
                ))
                .collect::<String>()
        ),
        Layout::PlainDivs => tiers
            .iter()
            .map(|t| {
                format!(
                    "<div class=\"{}\"><div>{}</div>{}</div>",
                    classes.block,
                    t.label,
                    with_notes(t, "div", classes.text)
                )
            })
            .collect::<String>(),
        Layout::SpanRows => tiers
            .iter()
            .map(|t| {
                format!(
                    "<p class=\"{}\"><span>{}</span>{}</p>",
                    classes.block,
                    t.label,
                    with_notes(t, "span", classes.text)
                )
            })
            .collect::<String>(),
        Layout::NestedList => format!(
            "<ul class=\"{}\">{}</ul>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!(
                    "<li>{}<ul>{}</ul></li>",
                    t.label,
                    with_notes(t, "li", classes.text)
                ))
                .collect::<String>()
        ),
        Layout::ImageAlt => format!(
            "<ul class=\"{}\">{}</ul>",
            classes.block,
            tiers
                .iter()
                .map(|t| format!(
                    "<li><img src=\"tier.png\" alt=\"{}\">{}</li>",
                    t.label,
                    with_notes(t, "span", classes.text)
                ))
                .collect::<String>()
        ),
        Layout::AriaGroups => tiers
            .iter()
            .map(|t| {
                format!(
                    "<div class=\"{}\" role=\"group\" aria-label=\"{}\">{}</div>",
                    classes.block,
                    t.label,
                    with_notes(t, "p", classes.text)
                )
            })
            .collect::<String>(),
        Layout::DeepHeadings => tiers
            .iter()
            .map(|t| {
                format!(
                    "<div class=\"{}\"><h3>{}</h3><h4>혜택</h4><h5>상세</h5>{}</div>",
                    classes.block,
                    t.label,
                    with_notes(t, "p", classes.text)
                )
            })
            .collect::<String>(),
    };
    let banner = banner
        .map(|b| format!("<div class=\"banner\">{b}</div>"))
        .unwrap_or_default();
    let (pad_before, pad_after) = if long {
        (
            format!("<p>{FILLER}</p><p>{FILLER}</p>"),
            format!("<p>{FILLER}</p>"),
        )
    } else {
        (String::new(), String::new())
    };
    let section = format!(
        "<section><h2>학생 혜택 안내</h2>{banner}{pad_before}{body}{extra}{pad_after}</section>"
    );
    let content = if wrap {
        format!("<main class=\"page-v2\"><div class=\"inner\"><div class=\"content\">{section}</div></div></main>")
    } else {
        section
    };
    format!("<html><head><title>t</title></head><body><nav><a href=\"/\">홈</a></nav>{content}<footer>고객센터 1588-0000</footer></body></html>")
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as usize
    }
}

struct Variant {
    layout: Layout,
    edit: Edit,
    before: String,
    after: String,
    quote: String,
    truth: Truth,
}

fn variants() -> Vec<Variant> {
    let mut rng = Rng(0x5eed_c0de_2026_1018);
    let mut out = Vec::new();
    for layout in LAYOUTS {
        for labels in LABEL_SETS {
            for template in &TEMPLATES {
                for same_values in [false, true] {
                    for edit in EDITS {
                        let target = rng.next() % 3;
                        let long = rng.next().is_multiple_of(3);
                        let shown = |i: usize| {
                            if same_values {
                                template.values[0]
                            } else {
                                template.values[i]
                            }
                        };
                        let sentence = |v: &str| template.text.replace("{v}", v);
                        let tier = |i: usize| Tier {
                            label: labels[i],
                            text: sentence(shown(i).0),
                            inside: None,
                            after: None,
                            before: None,
                        };
                        let base: Vec<Tier> = (0..3).map(tier).collect();
                        let quote = base[target].text.clone();
                        let before =
                            render(layout, &base, &CLASSES[0], target, "", None, false, long);
                        let mut tiers = base.clone();
                        let mut classes = &CLASSES[0];
                        let mut active = target;
                        let mut extra = String::new();
                        let mut banner: Option<String> = None;
                        let mut wrap = false;
                        let phrases = if template.english {
                            EN_PHRASES
                        } else {
                            KO_PHRASES
                        };
                        let phrase = phrases[rng.next() % phrases.len()].to_string();
                        let reformat_all = |tiers: &mut Vec<Tier>| {
                            for (i, t) in tiers.iter_mut().enumerate() {
                                let value =
                                    shown(labels.iter().position(|l| *l == t.label).unwrap_or(i)).0;
                                if t.text == sentence(value) {
                                    t.text = sentence(&(template.reformat)(value));
                                }
                            }
                        };
                        let swap = |tiers: &mut Vec<Tier>, active: &mut usize| {
                            let other = (target + 1) % 3;
                            let (a, b) = (tiers[target].label, tiers[other].label);
                            tiers[target].label = b;
                            tiers[other].label = a;
                            tiers.swap(target, other);
                            *active = target;
                            // The target label now sits on the other tier's
                            // value.
                            if same_values {
                                Truth::Same
                            } else {
                                Truth::Changed(template.values[other].1)
                            }
                        };
                        let truth = match edit {
                            Edit::Unchanged => Truth::Same,
                            Edit::RenameClasses => {
                                classes = &CLASSES[1];
                                Truth::Same
                            }
                            Edit::Reorder => {
                                tiers.reverse();
                                active = 2 - target;
                                Truth::Same
                            }
                            Edit::MoveToNewPage => {
                                wrap = true;
                                Truth::Same
                            }
                            Edit::Reformat => {
                                reformat_all(&mut tiers);
                                Truth::Same
                            }
                            Edit::DeleteTier => {
                                tiers.remove(target);
                                active = 0;
                                Truth::Gone
                            }
                            Edit::ChangeValue => {
                                tiers[target].text = sentence(template.fresh.0);
                                Truth::Changed(template.fresh.1)
                            }
                            Edit::ChangeAudience => {
                                tiers[target].text =
                                    template.audience.replace("{v}", shown(target).0);
                                Truth::Gone
                            }
                            Edit::PhraseInStatement => {
                                tiers[target].inside = Some(phrase.clone());
                                Truth::Gone
                            }
                            Edit::PhraseBesideAfter => {
                                tiers[target].after = Some(phrase.clone());
                                Truth::Gone
                            }
                            Edit::PhraseBesideBefore => {
                                tiers[target].before = Some(phrase.clone());
                                Truth::Gone
                            }
                            Edit::SectionBanner => {
                                banner = Some(phrase.clone());
                                Truth::Gone
                            }
                            Edit::SwapLabels => swap(&mut tiers, &mut active),
                            Edit::HiddenOldCopy => {
                                tiers[target].text = sentence(template.fresh.0);
                                extra = format!(
                                    "<div class=\"tab-pane\"><h3>{}</h3><p>{}</p></div>",
                                    labels[target], quote
                                );
                                Truth::Changed(template.fresh.1)
                            }
                            Edit::PastEndDate => {
                                tiers[target].after =
                                    Some(if template.english { EN_PAST } else { KO_PAST }.into());
                                Truth::Gone
                            }
                            Edit::ReformatAndDelete => {
                                reformat_all(&mut tiers);
                                tiers.remove(target);
                                active = 0;
                                Truth::Gone
                            }
                            Edit::ReformatAndChange => {
                                reformat_all(&mut tiers);
                                tiers[target].text =
                                    sentence(&(template.reformat)(template.fresh.0));
                                Truth::Changed(template.fresh.1)
                            }
                            Edit::ReformatAndSwap => {
                                reformat_all(&mut tiers);
                                swap(&mut tiers, &mut active)
                            }
                        };
                        let after = render(
                            layout,
                            &tiers,
                            classes,
                            active,
                            &extra,
                            banner.as_deref(),
                            wrap,
                            long,
                        );
                        out.push(Variant {
                            layout,
                            edit,
                            before,
                            after,
                            quote,
                            truth,
                        });
                    }
                }
            }
        }
    }
    out
}

#[derive(Default, Debug, Clone)]
struct Tally {
    variants: usize,
    confirmed: usize,
    false_hides: usize,
    correct_updates: usize,
    missed_updates: usize,
    false_confirms: usize,
    wrong_updates: usize,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.variants += other.variants;
        self.confirmed += other.confirmed;
        self.false_hides += other.false_hides;
        self.correct_updates += other.correct_updates;
        self.missed_updates += other.missed_updates;
        self.false_confirms += other.false_confirms;
        self.wrong_updates += other.wrong_updates;
    }
}

/// The reading a proposal gives, compared with the literal expectation:
/// digits and unit only, independent of the implementation's formatting.
fn reading_matches(proposed: &str, expected: &str) -> bool {
    proposed == expected
}

#[test]
fn generated_corpus_has_no_false_confirm_and_no_wrong_update() {
    let started = std::time::Instant::now();
    let corpus = variants();
    assert!(
        corpus.len() >= 5_000,
        "corpus has {} variants",
        corpus.len()
    );
    let mut by_edit: BTreeMap<Edit, Tally> = BTreeMap::new();
    let mut by_layout: BTreeMap<Layout, Tally> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    let mut setup_examples: Vec<String> = Vec::new();
    let mut setup_failures = 0;
    for variant in &corpus {
        let mut tally = Tally {
            variants: 1,
            ..Tally::default()
        };
        let before = view(&variant.before);
        let fact = confirm_in(
            &before,
            &variant.quote,
            Ages {
                min: None,
                max: None,
            },
        );
        let baseline = fact.as_ref().map(|f| resolve_in(f, &before));
        match (&fact, &baseline) {
            (Some(fact), Some(baseline)) if baseline.confirms() => {
                let resolution = resolve_in(fact, &view(&variant.after));
                match (&variant.truth, &resolution) {
                    (Truth::Same, r) if r.confirms() => tally.confirmed += 1,
                    (Truth::Same, _) => tally.false_hides += 1,
                    (_, r) if r.confirms() => {
                        tally.false_confirms += 1;
                        if failures.len() < 12 {
                            failures.push(format!(
                                "FALSE CONFIRM {:?}/{:?} {:?}: {:?}\n  quote {}\n  after {}",
                                variant.layout,
                                variant.edit,
                                variant.truth,
                                r,
                                variant.quote,
                                variant.after
                            ));
                        }
                    }
                    (Truth::Changed(expected), Resolution::Proposed(p))
                        if reading_matches(&p.new_signature, expected) =>
                    {
                        tally.correct_updates += 1
                    }
                    (_, Resolution::Proposed(p)) => {
                        tally.wrong_updates += 1;
                        if failures.len() < 12 {
                            failures.push(format!(
                                "WRONG UPDATE {:?}/{:?} {:?}: {:?}\n  quote {}\n  after {}",
                                variant.layout,
                                variant.edit,
                                variant.truth,
                                p,
                                variant.quote,
                                variant.after
                            ));
                        }
                    }
                    (Truth::Changed(_), _) => tally.missed_updates += 1,
                    (Truth::Gone, _) => {}
                }
            }
            _ => {
                setup_failures += 1;
                if setup_examples.len() < 5 {
                    setup_examples.push(format!(
                        "setup {:?}/{:?}: {:?} | {}",
                        variant.layout, variant.edit, baseline, variant.before
                    ));
                }
                continue;
            }
        }
        by_edit.entry(variant.edit).or_default().add(&tally);
        by_layout.entry(variant.layout).or_default().add(&tally);
    }
    let row = |name: String, t: &Tally| {
        eprintln!(
            "{:<20} {:>8} {:>9} {:>10} {:>11} {:>10} {:>10} {:>9}",
            name,
            t.variants,
            t.confirmed,
            t.false_hides,
            t.correct_updates,
            t.missed_updates,
            t.false_confirms,
            t.wrong_updates
        );
    };
    let header = "                     variants confirmed false_hide correct_upd missed_upd FALSE_CONF WRONG_UPD";
    eprintln!("by edit{header}");
    let mut total = Tally::default();
    for (edit, t) in &by_edit {
        row(format!("{edit:?}"), t);
        total.add(t);
    }
    eprintln!("by layout{header}");
    for (layout, t) in &by_layout {
        row(format!("{layout:?}"), t);
    }
    eprintln!(
        "total {total:?}, setup failures {setup_failures}, {:.1}s",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(setup_failures, 0, "{setup_examples:#?}");
    // The floor is a named limit that is 0 today; clippy reads `<= 0` as absurd.
    #[allow(clippy::absurd_extreme_comparisons)]
    let within = total.false_confirms <= super::FALSE_CONFIRM_FLOOR;
    assert!(within, "{failures:#?}");
    assert_eq!(total.wrong_updates, 0, "{failures:#?}");
}

/// The pinned first-sight readings of the generated corpus: one character
/// per variant whose offer is gone, `1` when the first-sight reader hides
/// the after-page, `0` when it shows it.
const FIRST_SIGHT_FLOOR: &str = "tests/fixtures/first_sight_floor.txt";

/// The first-sight differential: every gone offer the first-sight reader
/// hid when [`FIRST_SIGHT_FLOOR`] was written stays hidden. A reader change
/// may hide more (rewrite the file with `WRITE_FIRST_SIGHT_FLOOR=1`), never
/// fewer.
#[test]
fn the_first_sight_reader_never_shows_a_gone_offer_it_hid() {
    let empty = Anchors::default();
    let readings: String = variants()
        .iter()
        .filter(|v| matches!(v.truth, Truth::Gone))
        .map(|v| {
            let fact = StoredFact {
                quote: &v.quote,
                baseline: None,
                ages: Ages {
                    min: None,
                    max: None,
                },
                adoption_open: true,
                anchors: &empty,
            };
            if resolve(&view(&v.after), &fact).confirms() {
                '0'
            } else {
                '1'
            }
        })
        .collect();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIRST_SIGHT_FLOOR);
    if std::env::var_os("WRITE_FIRST_SIGHT_FLOOR").is_some() {
        std::fs::write(&path, format!("{readings}\n")).expect("floor written");
    }
    let pinned = std::fs::read_to_string(&path).expect("floor pinned");
    let pinned = pinned.trim_end();
    assert_eq!(
        pinned.len(),
        readings.len(),
        "the corpus changed; rewrite the floor"
    );
    let loosened: Vec<usize> = pinned
        .bytes()
        .zip(readings.bytes())
        .enumerate()
        .filter(|(_, (was, now))| *was == b'1' && *now == b'0')
        .map(|(i, _)| i)
        .collect();
    eprintln!(
        "first sight: {} gone variants, {} hidden, {} shown, {} loosened",
        readings.len(),
        readings.bytes().filter(|b| *b == b'1').count(),
        readings.bytes().filter(|b| *b == b'0').count(),
        loosened.len()
    );
    assert!(
        loosened.is_empty(),
        "now shown at first sight: {loosened:?}"
    );
}
