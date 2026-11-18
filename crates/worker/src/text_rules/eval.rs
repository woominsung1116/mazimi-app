//! Held-out measurement of rule_v1 against two independent label sets.
//!
//! - A: 300 programmes (`labels_a.jsonl`), ranks 1-150 were the prototype's
//!   development set, ranks 151-300 are held out.
//! - B: 100 programmes labelled by a second annotator with its own schema
//!   (`labels_b.jsonl`), a subset of A's sample (72 in A's development
//!   ranks, 28 in A's held-out ranks).
//!
//! Fixtures are generated from the annotators' files; only the eligibility
//! fields rule_v1 reads are kept, with every label whose quote lies in them.
//! A fact counts as correct when a label of the same kind overlaps its quote,
//! is about the applicant, has the same value and a compatible role (the
//! prototype's `eval_rules.py`, ported). The new kinds (homeless, marital,
//! children) were written from catalogue rows outside both samples.
//!
//! - C: see [`LABELS_C`].
//!
//! `cargo test -p mazimi-worker --lib text_rules::eval -- --nocapture`
//! prints the per-kind table; the gate test fails when a kind in
//! [`super::compile::ENABLED_KINDS`] is under [`MIN_PRECISION`] on either
//! held-out set or has no held-out evidence.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

use super::compile::ENABLED_KINDS;
use super::extract::{extract, Fact, Role};

pub const MIN_PRECISION: f64 = 0.98;

thread_local! {
    /// Count only `subject: app` labels as the applicant's (the prototype's
    /// scoring), for the strict columns of the report.
    static STRICT_SUBJECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const PROGRAMMES: &str = include_str!("../../tests/fixtures/condrules/programmes.jsonl");
const LABELS_A: &str = include_str!("../../tests/fixtures/condrules/labels_a.jsonl");
const LABELS_B: &str = include_str!("../../tests/fixtures/condrules/labels_b.jsonl");
/// C: 75 catalogue programmes outside A's sample and outside the rows read
/// while writing the new kinds, chosen for 무주택 / 혼인 / 자녀 wording and
/// labelled by an annotator who never saw the rules (A's schema, with fixed
/// value fields for the new kinds).
const PROGRAMMES_C: &str = include_str!("../../tests/fixtures/condrules/programmes_c.jsonl");
const LABELS_C: &str = include_str!("../../tests/fixtures/condrules/labels_c.jsonl");

struct Programme {
    rank: i64,
    source_type: String,
    texts: BTreeMap<String, String>,
}

fn programmes(file: &str) -> HashMap<String, Programme> {
    file.lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).unwrap();
            let texts = v["texts"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, t)| (k.clone(), t.as_str().unwrap().to_string()))
                .collect();
            (
                v["id"].as_str().unwrap().to_string(),
                Programme {
                    rank: v["rank"].as_i64().unwrap(),
                    source_type: v["source_type"].as_str().unwrap().to_string(),
                    texts,
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Set {
    A,
    B,
    C,
}

#[derive(Debug, Default, Clone)]
pub struct KindScore {
    pub emitted: usize,
    pub correct: usize,
    pub labels: usize,
    pub hit: usize,
    /// Facts with an unclear role: never compiled, not scored.
    pub unknown_role: usize,
}

impl KindScore {
    pub fn precision(&self) -> Option<f64> {
        (self.emitted > 0).then(|| self.correct as f64 / self.emitted as f64)
    }
}

#[derive(Debug, Default, Clone)]
pub struct ProgrammeScore {
    pub programmes: usize,
    /// Complete with enabled kinds only (text side of "fully structured").
    pub claimed: usize,
    pub claimed_correct: usize,
    /// Claimed and every fact compiles to a profile condition.
    pub evaluable_claimed: usize,
    pub evaluable_correct: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub kinds: BTreeMap<&'static str, KindScore>,
    pub programmes: ProgrammeScore,
}

fn spans(text: &str, quote: &str) -> Vec<(usize, usize)> {
    text.match_indices(quote)
        .map(|(i, q)| (i, i + q.len()))
        .collect()
}

fn overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn same_major(fact: &Value, label: &Value) -> bool {
    let ours = fact["major"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('학');
    !ours.is_empty() && label["major"].as_str().unwrap_or_default().contains(ours)
}

fn has(list: &Value, item: &str) -> bool {
    match list {
        Value::Array(items) => items.iter().any(|v| v.as_str() == Some(item)),
        Value::String(s) => s == item,
        _ => false,
    }
}

/// Value agreement of a fact with a label of the same kind.
fn value_ok(set: Set, kind: &str, fact: &Value, label: &Value) -> bool {
    match kind {
        "age" => fact["min"] == label["min"] && fact["max"] == label["max"],
        "income_median" => {
            fact["max_pct"] == label["max_pct"]
                && fact["strict"].as_bool().unwrap_or(false)
                    == label["strict"].as_bool().unwrap_or(false)
        }
        "income_kosaf" => fact["max"] == label["max"],
        "major" => same_major(fact, label),
        "business" => match set {
            Set::A | Set::C => fact["type"] == label["type"],
            Set::B => match fact["type"].as_str() {
                Some("prospective_founder") => has(&label["status"], "prospective_founder"),
                Some("small_business") => has(&label["status"], "small_business_owner"),
                Some("sme") => has(&label["firm"], "sme"),
                Some("startup_within_years") => {
                    label["max_years_since_founding"] == fact["years"]
                        || has(&label["status"], "startup")
                }
                Some("self_employed") => {
                    has(&label["legal_form"], "sole_proprietor")
                        || has(&label["status"], "business_owner")
                }
                _ => false,
            },
        },
        "marital" if set == Set::C => {
            fact["status"] == label["status"]
                && match fact.get("newlywed_years") {
                    None => label["max_years_married"].is_null(),
                    Some(years) => *years == label["max_years_married"],
                }
        }
        "homeless" if set == Set::C => fact["level"] == label["level"],
        "children" if set == Set::C => {
            fact["min"] == label["min"] && label["age_limited"] != Value::Bool(true)
        }
        "marital" => {
            let status = fact["status"].as_str();
            let label_status = label["status"]
                .as_str()
                .or_else(|| label["marital"].as_str())
                .or_else(|| (label["newlywed"] == Value::Bool(true)).then_some("married"));
            if status != label_status {
                return false;
            }
            match fact.get("newlywed_years") {
                None => true,
                Some(Value::Null) => {
                    label["newlywed"] == Value::Bool(true) || label_status == Some("married")
                }
                Some(years) => {
                    let months = years.as_i64().map(|y| y * 12);
                    label["max_months_married"].as_i64() == months
                        || label["since_months"]
                            .as_array()
                            .and_then(|a| a.last())
                            .and_then(Value::as_i64)
                            == months
                }
            }
        }
        "children" => fact["min"] == label["min"],
        // homeless (B: {homeless: true}), prior_receipt: kind only.
        _ => true,
    }
}

/// Kinds whose profile answer is about the user's household or marriage: a
/// label on the household or the couple is about the applicant's answer.
const HOUSEHOLD_KINDS: [&str; 3] = ["income_median", "homeless", "marital"];

fn applicant(set: Set, label: &Value) -> bool {
    let household = HOUSEHOLD_KINDS.contains(&kind_of(label))
        && matches!(label["subject"].as_str(), Some("household" | "couple"))
        && !STRICT_SUBJECT.with(|s| s.get());
    match set {
        Set::A | Set::C => label["subject"] == "app" || household,
        Set::B => matches!(
            label["who"].as_str(),
            None | Some("self") | Some("applicant")
        ),
    }
}

fn role_ok(set: Set, role: Role, label: &Value, logic: &str) -> bool {
    match set {
        Set::A | Set::C => {
            let lr = label["role"].as_str().unwrap_or_default();
            match role {
                Role::Req => lr == "req",
                Role::Alt => lr.starts_with("alt:") && lr != "alt:exc",
                Role::Excl => lr == "excl",
                Role::Unknown => false,
            }
        }
        Set::B => {
            let track = label["track"].as_str().is_some();
            match role {
                Role::Req => !track || logic == "all",
                Role::Alt => track || matches!(logic, "any" | "mixed"),
                Role::Excl => {
                    label["kind"] == "prior_receipt"
                        || label["value"].get("excludes").is_some()
                        || label["value"].get("excluded_industries").is_some()
                }
                Role::Unknown => false,
            }
        }
    }
}

fn kind_of(label: &Value) -> &str {
    label["kind"].as_str().unwrap_or_default()
}

/// Score one label set over the programmes `keep` selects.
fn score(set: Set, labels: &str, keep: impl Fn(i64) -> bool) -> Report {
    let progs = programmes(if set == Set::C {
        PROGRAMMES_C
    } else {
        PROGRAMMES
    });
    let mut report = Report::default();
    for line in labels.lines() {
        let lab: Value = serde_json::from_str(line).unwrap();
        let p = &progs[lab["id"].as_str().unwrap()];
        if !keep(p.rank) {
            continue;
        }
        let logic = lab["logic"].as_str().unwrap_or("all");
        let conds: Vec<&Value> = lab["conditions"].as_array().unwrap().iter().collect();
        let res = extract(&p.source_type, &p.texts);
        let mut matched = vec![false; conds.len()];
        let mut programme_fp = 0;
        for f in &res.facts {
            let entry = report.kinds.entry(f.kind).or_default();
            if f.role == Role::Unknown {
                entry.unknown_role += 1;
                continue;
            }
            entry.emitted += 1;
            let text = &p.texts[&f.field];
            let fspan = (f.start, f.start + f.quote.len());
            let cands: Vec<usize> = (0..conds.len())
                .filter(|i| {
                    kind_of(conds[*i]) == f.kind
                        && conds[*i]["fields"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|x| x == f.field.as_str())
                        && spans(text, conds[*i]["quote"].as_str().unwrap())
                            .into_iter()
                            .any(|s| overlap(fspan, s))
                })
                .collect();
            let correct = if cands.is_empty() {
                // The same fact restated elsewhere in the field.
                (0..conds.len()).any(|i| {
                    let c = conds[i];
                    kind_of(c) == f.kind
                        && c["fields"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|x| x == f.field.as_str())
                        && applicant(set, c)
                        && value_ok(set, f.kind, &f.value, &c["value"])
                        && role_ok(set, f.role, c, logic)
                })
            } else {
                let first = conds[cands[0]];
                let fact_ok =
                    applicant(set, first) && value_ok(set, f.kind, &f.value, &first["value"]);
                let mut any_role = false;
                for i in &cands {
                    if fact_ok && role_ok(set, f.role, conds[*i], logic) {
                        matched[*i] = true;
                        any_role = true;
                    }
                }
                fact_ok && any_role
            };
            if correct {
                entry.correct += 1;
            } else {
                programme_fp += 1;
                if std::env::var("RULE_V1_ERRORS").is_ok() {
                    eprintln!(
                        "FP {:?} rank {} {} {:?} {:?} {}",
                        set, p.rank, f.kind, f.role, f.value, f.quote
                    );
                }
            }
        }
        for (i, c) in conds.iter().enumerate() {
            let kind = kind_of(c);
            if !applicant(set, c) || set != Set::B && c["role"] == "tier" {
                continue;
            }
            if ENABLED_KINDS.contains(&kind) {
                let entry = report.kinds.entry(static_kind(kind)).or_default();
                entry.labels += 1;
                entry.hit += matched[i] as usize;
            }
        }
        // Programme claim, text side, enabled kinds only.
        let enabled_facts: Vec<&Fact> = res
            .facts
            .iter()
            .filter(|f| ENABLED_KINDS.contains(&f.kind))
            .collect();
        let claimed = res.complete && enabled_facts.len() == res.facts.len();
        report.programmes.programmes += 1;
        if claimed {
            let all_hit = conds
                .iter()
                .enumerate()
                .filter(|(_, c)| !(set != Set::B && c["role"] == "tier"))
                .all(|(i, _)| matched[i]);
            let correct = lab["fully_structurable"] == Value::Bool(true)
                && lab["outside"].as_u64() == Some(0)
                && all_hit
                && programme_fp == 0
                && conds.iter().all(|c| applicant(set, c));
            report.programmes.claimed += 1;
            report.programmes.claimed_correct += correct as usize;
            let evaluable = enabled_facts
                .iter()
                .all(|f| f.role == Role::Req && super::compile::leaves(f).is_some());
            if evaluable {
                report.programmes.evaluable_claimed += 1;
                report.programmes.evaluable_correct += correct as usize;
            }
            if !correct && std::env::var("RULE_V1_ERRORS").is_ok() {
                eprintln!("WRONG_COMPLETE {:?} rank {}", set, p.rank);
            }
        }
    }
    report
}

fn static_kind(kind: &str) -> &'static str {
    ENABLED_KINDS
        .iter()
        .copied()
        .find(|k| *k == kind)
        .unwrap_or("other")
}

pub fn held_out_a() -> Report {
    score(Set::A, LABELS_A, |rank| rank > 150)
}
pub fn dev_a() -> Report {
    score(Set::A, LABELS_A, |rank| rank <= 150)
}
pub fn all_b() -> Report {
    score(Set::B, LABELS_B, |_| true)
}
pub fn b_in_a_held_out() -> Report {
    score(Set::B, LABELS_B, |rank| rank > 150)
}
pub fn all_c() -> Report {
    score(Set::C, LABELS_C, |_| true)
}

fn print(name: &str, report: &Report) {
    println!("== {name}");
    println!(
        "{:16} {:>5} {:>4} {:>7} {:>5} {:>4} {:>4}",
        "kind", "emit", "ok", "prec", "label", "hit", "unk"
    );
    for (kind, s) in &report.kinds {
        println!(
            "{:16} {:>5} {:>4} {:>7} {:>5} {:>4} {:>4}",
            kind,
            s.emitted,
            s.correct,
            s.precision().map_or("-".into(), |p| format!("{p:.3}")),
            s.labels,
            s.hit,
            s.unknown_role
        );
    }
    let p = &report.programmes;
    println!(
        "programmes {} claimed_complete {} correct {} | evaluable {} correct {}",
        p.programmes, p.claimed, p.claimed_correct, p.evaluable_claimed, p.evaluable_correct
    );
}

#[test]
fn report() {
    print("A dev (ranks 1-150, prototype development set)", &dev_a());
    print("A held-out (ranks 151-300)", &held_out_a());
    print("B (100, independent annotator)", &all_b());
    print("B within A held-out (28)", &b_in_a_held_out());
    print("C (75, new-kind wording, independent annotator)", &all_c());
    STRICT_SUBJECT.with(|s| s.set(true));
    print(
        "C, strict subject (household/couple labels not the applicant's)",
        &all_c(),
    );
    print("A held-out, strict subject", &held_out_a());
    STRICT_SUBJECT.with(|s| s.set(false));
}

/// Every shipped kind has held-out evidence and is at least 98% precise on
/// both held-out sets.
#[test]
fn shipped_kinds_meet_the_precision_gate() {
    let a = held_out_a();
    let b = all_b();
    let c = all_c();
    for kind in ENABLED_KINDS {
        let (sa, sb, sc) = (
            a.kinds.get(kind).cloned().unwrap_or_default(),
            b.kinds.get(kind).cloned().unwrap_or_default(),
            c.kinds.get(kind).cloned().unwrap_or_default(),
        );
        assert!(
            sa.emitted + sb.emitted + sc.emitted > 0,
            "{kind}: no held-out evidence; it must be disabled"
        );
        for (name, s) in [("A held-out", &sa), ("B", &sb), ("C", &sc)] {
            if let Some(p) = s.precision() {
                assert!(p >= MIN_PRECISION, "{kind}: {name} precision {p:.3}");
            }
        }
    }
    // A complete-and-evaluable claim is never contradicted on held-out text.
    for (name, r) in [("A held-out", &a), ("B", &b), ("C", &c)] {
        assert_eq!(
            r.programmes.evaluable_correct, r.programmes.evaluable_claimed,
            "{name}: an evaluable programme claim is wrong"
        );
    }
}

/// The review's 95 adversarial sentences (exclusions, negations, 우선/포함
/// notes, head counts, placeholders, OR wording, 연 나이). None outside the
/// short list of plainly structured sentences may come out evaluable.
#[test]
fn adversarial_sentences_fail_closed() {
    const ADVERSARIAL: &str = include_str!("../../tests/fixtures/condrules/adversarial.jsonl");
    let mut open = Vec::new();
    for line in ADVERSARIAL.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let mut payload = row["context"].clone();
        let texts: BTreeMap<String, String> = row["texts"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                payload[k] = v.clone();
                (k.clone(), v.as_str().unwrap().to_string())
            })
            .collect();
        let source = row["source_type"].as_str().unwrap();
        let compiled = super::compile::compile(
            &extract(source, &texts),
            &super::coverage::uncovered_codes(source, &payload),
        );
        if compiled.evaluable && row["may_be_evaluable"] != Value::Bool(true) {
            open.push(format!("{} {}: {:?}", row["set"], row["id"], texts));
        }
    }
    assert!(open.is_empty(), "fail open:\n{}", open.join("\n"));
}
