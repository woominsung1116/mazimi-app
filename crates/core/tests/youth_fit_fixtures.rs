//! `youth_fit` against the hand-labelled rows of the 2026-10-21 relevance
//! review (`tests/fixtures/youth_fit/`). Each fixture row carries its stored
//! columns, the raw-payload fields the classifier reads, a human label and
//! the verdict of the reviewed Python prototype (`reference`).

use mazimi_core::youth_fit::{self, ProgramFacts, Verdict};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Fixture {
    rows: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    n: i64,
    source_type: String,
    title: String,
    summary: Option<String>,
    min_age: Option<i32>,
    max_age: Option<i32>,
    curated_youth: bool,
    raw: Value,
    label: String,
    #[serde(default)]
    note: String,
    reference: Reference,
}

#[derive(Deserialize)]
struct Reference {
    verdict: String,
    rule: Option<String>,
}

fn load(name: &str) -> Vec<Row> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/youth_fit")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str::<Fixture>(&text).unwrap().rows
}

fn decide(row: &Row) -> youth_fit::Decision {
    youth_fit::classify(&ProgramFacts {
        source_type: &row.source_type,
        title: &row.title,
        summary: row.summary.as_deref(),
        min_age: row.min_age,
        max_age: row.max_age,
        curated_youth: row.curated_youth,
        raw: &row.raw,
    })
}

/// Every row reaches the reference verdict and, for a demoted row, the same
/// rule.
fn assert_reference(rows: &[Row]) {
    let mismatches: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let d = decide(row);
            let same = d.verdict.as_str() == row.reference.verdict
                && (!d.verdict.demoted() || Some(d.rule) == row.reference.rule.as_deref());
            (!same).then(|| {
                format!(
                    "#{} {}: got {} {} ({:?}), reference {} {:?}",
                    row.n,
                    row.title,
                    d.verdict.as_str(),
                    d.rule,
                    d.evidence,
                    row.reference.verdict,
                    row.reference.rule
                )
            })
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn labelled_sample_reproduces_the_reference_verdicts() {
    assert_reference(&load("sample200"));
}

#[test]
fn labelled_sample_reaches_the_reported_precision_and_recall() {
    let rows = load("sample200");
    assert_eq!(rows.len(), 200);
    let strict = |label: &str| matches!(label, "F" | "B" | "O" | "E");
    let (mut tp, mut fp, mut fn_, mut youth_demoted) = (0, 0, 0, Vec::new());
    for row in &rows {
        let d = decide(row);
        let flagged = matches!(d.verdict, Verdict::OffTarget | Verdict::BusinessOnly);
        match (flagged, strict(&row.label)) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fn_ += 1,
            _ => {}
        }
        // Disability and veteran rows are shown to users with that flag, so a
        // "Y" row in those tiers is not lost to the people it serves.
        if d.verdict.demoted()
            && d.verdict != Verdict::DisabilityOnly
            && d.verdict != Verdict::VeteranOnly
            && matches!(row.label.as_str(), "Y" | "N")
        {
            youth_demoted.push(format!("#{} {}", row.n, row.title));
        }
    }
    // Report §3 accuracy was TP 44, FP 0, FN 4. One F row (귀농인 농기계)
    // is now kept on purpose: individual 귀농 benefits are youth relevant.
    assert_eq!((tp, fp, fn_), (43, 0, 5));
    assert!(
        youth_demoted.is_empty(),
        "youth rows demoted: {youth_demoted:?}"
    );
}

/// Holdouts 3-10 were each drawn blind from the rows the rules of their day
/// demoted, then labelled R (rightly demoted), W (a young individual is a
/// real target), B (borderline: a young person qualifies only through an
/// OR'd group or as a parent) or P (a youth row demoted on purpose because
/// only an organisation applies). Holdouts 3-9 then tuned the rules, so for
/// them this is a regression guard: the misses still demoted stay the known
/// ones, each for a stated reason.
#[test]
fn holdouts_keep_only_the_known_misses_demoted() {
    let expected: &[(&str, &[i64])] = &[
        // 자활지원 and 학교밖청소년 are typed org-only (no individual applies);
        // 달성 장학금(지역사회 공헌) stays a pupil row.
        ("holdout3", &[18, 40, 121]),
        ("holdout4", &[]),
        ("holdout5", &[]),
        ("holdout6", &[]),
        // 어르신 및 장애인: disability tier; 28청춘창업소: org-only.
        ("holdout7", &[47, 105]),
        // 도시철도 요금 면제 (disability tier), 폐지수집 어르신, 차별 개선
        // 센터 and 내일채움공제 (org-only), 다문화 통·번역 (pupil row).
        ("holdout8", &[61, 116, 122, 135, 164]),
        ("holdout9", &[59, 89, 127, 165, 173, 180]),
        ("holdout10", &[68, 110, 132, 133, 140, 146, 194, 198]),
    ];
    for (name, misses) in expected {
        let rows = load(name);
        assert_reference(&rows);
        let still: Vec<i64> = rows
            .iter()
            .filter(|row| row.label != "R" && decide(row).verdict.demoted())
            .map(|row| row.n)
            .collect();
        assert_eq!(&still, misses, "{name}");
    }
}

/// The final blind holdout, drawn after the product owner's revisions. Its 4
/// clear misses were disability rows that also list 한부모 or families;
/// excluding those groups from the disability tier only un-demotes rows, so
/// the measured 4 W / 7 B of 200 are upper bounds. With the final rules no
/// clear miss is demoted.
#[test]
fn final_holdout_wrongly_demotes_no_youth_target() {
    let rows = load("holdout10");
    assert_eq!(rows.len(), 200);
    let count = |label: &str| rows.iter().filter(|r| r.label == label).count();
    assert_eq!((count("W"), count("B"), count("P")), (4, 7, 2));
    let demoted: Vec<&Row> = rows
        .iter()
        .filter(|r| decide(r).verdict.demoted())
        .collect();
    let of = |label: &str| demoted.iter().filter(|r| r.label == label).count();
    assert_eq!((demoted.len(), of("W"), of("B"), of("P")), (193, 0, 6, 2));
    for row in rows.iter().filter(|r| r.label != "R") {
        assert!(!row.note.is_empty(), "#{} has no reason", row.n);
    }
}

/// The production blind holdout, drawn after the second review: 300 demoted
/// rows unseen by any earlier holdout. Its 2 clear misses are bad source data
/// (귀농인 이사비용 stores its "65세 이하" ceiling as the floor; 발전소주변
/// 지역 전기요금 is typed org-only), so no rule was tuned on them. The kept
/// sample measures off-target rows that stay in the list.
#[test]
fn production_holdout_measures_false_demotion_and_kept_off_target() {
    let rows = load("holdout11");
    assert_reference(&rows);
    assert_eq!(rows.len(), 300);
    let count = |rows: &[Row], label: &str| rows.iter().filter(|r| r.label == label).count();
    assert_eq!((count(&rows, "W"), count(&rows, "B")), (2, 9));
    // The second review's D1 fix keeps one borderline row (예방접종 서비스:
    // 수급자 listed as a group of their own); every other row stays demoted.
    let kept_now: Vec<i64> = rows
        .iter()
        .filter(|r| !decide(r).verdict.demoted())
        .map(|r| r.n)
        .collect();
    assert_eq!(kept_now, [8]);

    let kept = load("kept11");
    assert_reference(&kept);
    assert_eq!(kept.len(), 100);
    // All kept. #52 (제대군인, labelled O) is not a veteran-family row
    // (review2 M1): the list caps it through an audience group no profile
    // answers instead of the veteran tier.
    let demoted: Vec<i64> = kept
        .iter()
        .filter(|r| decide(r).verdict.demoted())
        .map(|r| r.n)
        .collect();
    assert!(demoted.is_empty(), "{demoted:?}");
    assert_eq!((count(&kept, "O"), count(&kept, "B")), (4, 7));
    for row in rows
        .iter()
        .chain(&kept)
        .filter(|r| !matches!(r.label.as_str(), "R" | "Y"))
    {
        assert!(!row.note.is_empty(), "#{} has no reason", row.n);
    }
}

/// The second re-review's cases: its 2 wrong demotions (D1 on rows that
/// also name residents or recipients on their own), the 지역사회서비스투자사업
/// child-ceiling row that names 노인 and 일반시민 보장구 rows are kept; the
/// near misses beside them (disabled residents, a resident preference,
/// recipients tied to the disability, 북한이탈주민, 후계농 in a target)
/// stay demoted.
#[test]
fn second_review_cases_keep_named_groups_and_hold_near_misses() {
    let rows = load("review2");
    assert_reference(&rows);
    let wrong: Vec<String> = rows
        .iter()
        .filter(|r| decide(r).verdict.demoted() != (r.label == "R"))
        .map(|r| format!("#{} {} ({})", r.n, r.title, r.label))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    assert_eq!(rows.iter().filter(|r| r.label == "W").count(), 5);
}

/// Verdicts over a whole catalogue snapshot, one JSON row per line: either
/// the relevance review's `catalog.jsonl` format (`src`, `g_target`, …) or
/// rows that carry the stored columns and a `raw` object of [`RAW_KEYS`]
/// (`source_type`, `title`, `summary`, `min_age`, `max_age`,
/// `curated_youth`, `raw`). Writes `id, verdict, rule, evidence` per row. Run
/// with `YOUTH_FIT_CATALOG=/path/rows.jsonl YOUTH_FIT_OUT=/path/out.tsv
/// cargo test -p mazimi-core --test youth_fit_fixtures -- --ignored`.
///
/// [`RAW_KEYS`]: mazimi_core::youth_fit::RAW_KEYS
#[test]
#[ignore]
fn catalogue_snapshot_verdicts() {
    use std::io::{BufRead, Write};
    let path = std::env::var("YOUTH_FIT_CATALOG").expect("YOUTH_FIT_CATALOG");
    let out = std::env::var("YOUTH_FIT_OUT").expect("YOUTH_FIT_OUT");
    let mut out = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    for line in file.lines() {
        let line = line.unwrap();
        if !line.starts_with('{') {
            continue;
        }
        let r: Value = serde_json::from_str(&line).unwrap();
        let raw = match r.get("raw") {
            Some(raw) => raw.clone(),
            None => {
                let mut raw = serde_json::Map::new();
                for (key, field) in [
                    ("지원대상", "g_target"),
                    ("선정기준", "g_select"),
                    ("사용자구분", "g_user"),
                    ("서비스분야", "g_field"),
                    ("supportConditions", "g_sc"),
                    ("addAplyQlfcCndCn", "y_add"),
                    ("trgterIndvdlNmArray", "w_trg"),
                    ("lifeNmArray", "w_life"),
                ] {
                    if let Some(v) = r.get(field).filter(|v| !v.is_null()) {
                        raw.insert(key.to_owned(), v.clone());
                    }
                }
                Value::Object(raw)
            }
        };
        let title = text(&r, "title").unwrap_or_default();
        let source = text(&r, "source_type")
            .or_else(|| text(&r, "src"))
            .unwrap_or_default();
        let summary = text(&r, "summary");
        let age = |k: &str| r.get(k).and_then(Value::as_i64).map(|v| v as i32);
        let d = youth_fit::classify(&ProgramFacts {
            source_type: &source,
            title: &title,
            summary: summary.as_deref(),
            min_age: age("min_age"),
            max_age: age("max_age"),
            curated_youth: r
                .get("curated_youth")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            raw: &raw,
        });
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            text(&r, "id").unwrap_or_default(),
            d.verdict.as_str(),
            d.rule,
            d.evidence.unwrap_or_default()
        )
        .unwrap();
    }
}
