//! The search answer, its next actions and the grounding check. Fixed
//! templates filled from tool results only ("추천 설명은 템플릿 기반 우선"):
//! names, deadlines, conditions and counts are copied from results; no
//! sentence says the user is eligible (the rule engine's `verified` is the
//! only eligibility claim, and the answer still points to the notice).
//!
//! [`ungrounded`] is the response-side check: every quoted name, date,
//! D-day and amount in a line must appear in this run's results. `compose`
//! drops a line that fails it.
use super::{SuggestedAction, ToolResult};
use mazimi_core::agent::ToolName;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use uuid::Uuid;

/// The one not-final-eligibility line of a search answer that lists rows.
pub const NOT_FINAL: &str = "최종 자격은 공식 공고에서 확인해 주세요.";

/// Programmes named one by one in a search answer.
pub const NAMED: usize = 3;

/// Who the goal says the user is, for the condition line only ("대학생").
/// Display words; they never filter (the saved profile does).
const WHO: &[&str] = &[
    "대학원생",
    "대학생",
    "휴학생",
    "졸업생",
    "졸업예정자",
    "취준생",
    "취업준비생",
    "직장인",
    "사회초년생",
    "고등학생",
    "신혼부부",
];

pub fn who_label(goal: &str) -> Option<&'static str> {
    WHO.iter().copied().find(|w| goal.contains(w))
}

/// Short condition names from the recommendation reasons ("부산 거주 조건에
/// 맞아요." → "부산 거주"). Deadline reasons are left out: the D-day is shown
/// on its own.
pub fn fit_reasons(reasons: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for reason in reasons {
        if reason.contains("남았어요") || reason.contains("오늘 마감") {
            continue;
        }
        let base = reason
            .split(" 조건")
            .next()
            .unwrap_or(reason)
            .trim()
            .trim_end_matches('.');
        let short = match base {
            "현재 연령" | "연령" => "나이".to_owned(),
            b if b.starts_with("소득 구간") => "소득 구간".to_owned(),
            b => b.to_owned(),
        };
        if !short.is_empty() && short.chars().count() <= 20 && !out.contains(&short) {
            out.push(short);
        }
    }
    out
}

/// "11월 30일" from "2026-11-30" (or an RFC 3339 time, read in Korea).
pub(crate) fn month_day(value: &str) -> Option<String> {
    if let Ok(at) = value.parse::<chrono::DateTime<chrono::Utc>>() {
        let local = at + chrono::Duration::hours(9);
        return Some(local.format("%-m월 %-d일").to_string());
    }
    let date = chrono::NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
    Some(date.format("%-m월 %-d일").to_string())
}

/// "월 30만원 이상" for a search's amount bound (audit M6).
fn amount_phrase(args: &Value) -> Option<String> {
    let min = args["amount_min"].as_i64()?;
    Some(crate::services::grounded_qa::intent::amount_label(
        min,
        args["amount_period"].as_str(),
    ))
}

/// A search's deadline period as the goal said it: a named calendar period
/// as its Korean dates ("11월 10일~11월 15일", or one day), a length or an
/// implicit window as "N일 안".
fn period_phrase(args: &Value) -> Option<String> {
    let to = args["deadline_within_days"].as_u64()?;
    let from = args["deadline_from_days"].as_u64().unwrap_or(0).min(to);
    // The reading travels with the arguments; a window starting after
    // today is dates whatever was recorded.
    if from == 0 && args["deadline_named"] != true {
        return Some(format!("{to}일 안"));
    }
    let today = mazimi_core::seoul_date(chrono::Utc::now());
    let day = |offset: u64| {
        (today + chrono::Days::new(offset))
            .format("%-m월 %-d일")
            .to_string()
    };
    Some(if from == to {
        day(to)
    } else {
        format!("{}~{}", day(from), day(to))
    })
}

fn deadline_phrase(row: &Value) -> String {
    match (
        row["deadline"].as_str().and_then(month_day),
        row["deadline_days"].as_i64(),
    ) {
        (Some(date), Some(0)) => format!("마감 {date}(오늘 마감)"),
        (Some(date), Some(days)) if days > 0 => format!("마감 {date}(D-{days})"),
        (Some(date), _) => format!("마감 {date}"),
        (None, _) => "마감일 정보 없음".to_owned(),
    }
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Met, unmet and unknown condition labels of a `check_eligibility` result.
fn checked(result: &ToolResult) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut met, mut unmet, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    for check in result.data["checks"].as_array().into_iter().flatten() {
        let Some(label) = check["label"].as_str() else {
            continue;
        };
        let target = match check["status"].as_str() {
            Some("pass") => &mut met,
            Some("fail") => &mut unmet,
            // An unknown bundle names nothing to check; its missing profile
            // fields do (`row_line`).
            _ if check["bundle"] == true => continue,
            _ => &mut unknown,
        };
        if !target.iter().any(|l| l == label) {
            target.push(label.to_owned());
        }
    }
    (met, unmet, unknown)
}

/// "주거 · 부산 · 대학생" of a search result.
pub fn conditions(data: &Value, goal: &str) -> Vec<String> {
    let mut parts: Vec<String> = strings(&data["topic"], "labels");
    if parts.is_empty() {
        let types: Vec<&str> = data["args"]["program_types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|t| match t {
                "scholarship" => Some("장학금"),
                "policy" => Some("정책"),
                "corporate_benefit" | "campus_benefit" => Some("할인·혜택"),
                _ => None,
            })
            .collect();
        parts.extend(types.into_iter().map(str::to_owned));
    }
    parts.dedup();
    if let Some(region) = data["region_label"].as_str() {
        parts.push(region.to_owned());
    }
    if let Some(status) = data["intent"]["enrollment_label"].as_str() {
        parts.push(status.to_owned());
    } else if let Some(who) = who_label(goal) {
        parts.push(who.to_owned());
    }
    if let Some(period) = period_phrase(&data["args"]) {
        parts.push(format!("{period} 마감"));
    }
    if let Some(amount) = amount_phrase(&data["args"]) {
        parts.push(amount);
    }
    parts
}

/// The condition line shown on the search step ("주거 · 부산 · 대학생
/// 조건으로 찾았어요.").
pub fn topic_line(data: &Value, goal: &str) -> Option<String> {
    let parts = conditions(data, goal);
    (!parts.is_empty()).then(|| format!("{} 조건으로 찾았어요.", parts.join(" · ")))
}

fn relaxed_phrase(reason: &str) -> &'static str {
    match reason {
        "deadline" => "마감 기간",
        "program_type" => "혜택 종류",
        _ => "일부",
    }
}

/// The search part of the answer. `searches` are this run's search results
/// in order (a relaxed re-search after the first); `checks` the eligibility
/// results by programme.
pub fn search_lines(
    searches: &[&ToolResult],
    checks: &HashMap<Uuid, &ToolResult>,
    goal: &str,
) -> Vec<String> {
    let Some(last) = searches.iter().rev().find(|r| r.ok).copied() else {
        return Vec::new();
    };
    let data = &last.data;
    if data["required_profile_fields"]
        .as_array()
        .is_some_and(|f| !f.is_empty())
    {
        return vec!["혜택을 찾으려면 프로필에 출생연도와 거주 지역이 필요해요.".into()];
    }
    let mut lines: Vec<String> = Vec::new();
    let rows: Vec<&Value> = data["results"].as_array().into_iter().flatten().collect();
    let topic = &data["topic"];
    let labels = strings(topic, "labels").join("·");
    let on_topic_rows: Vec<&Value> = rows
        .iter()
        .copied()
        .filter(|r| r["topic_match"].as_str() != Some("mentions"))
        .collect();
    // The rows the answer shows, one block each: the same rows `found`
    // holds (re-review 3 L1). A fact every shown row shares (no deadline)
    // is said once, after them.
    let shown = shown_rows(data, checks);
    let no_deadlines = !shown.is_empty() && shown.iter().all(|(row, _)| row["deadline"].is_null());
    let found = if topic.is_object() {
        topic["on_topic"].as_u64().unwrap_or(0)
    } else {
        data["total_candidates"]
            .as_u64()
            .unwrap_or(rows.len() as u64)
    };
    if let Some(reason) = data["relaxed"].as_str() {
        let first = searches.first().map_or(0, |r| {
            r.data["topic"]["on_topic"]
                .as_u64()
                .unwrap_or_else(|| r.data["results"].as_array().map_or(0, Vec::len) as u64)
        });
        lines.push(format!(
            "처음 조건으로는 {first}개뿐이라 {} 조건을 풀어 다시 찾았어요.",
            relaxed_phrase(reason)
        ));
    }
    if topic.is_object() && found == 0 {
        lines.push(format!(
            "{labels} 분야에서 지금 조건에 맞는 혜택을 찾지 못했어요."
        ));
        if let (Some(label), Some(count)) = (
            topic["nearest"]["label"].as_str(),
            topic["nearest"]["count"].as_u64(),
        ) {
            lines.push(format!(
                "가까운 분야인 {label}에는 같은 조건의 혜택이 {count}개 있어요."
            ));
        }
    } else if rows.is_empty() {
        lines.push("조건에 맞는 혜택을 찾지 못했어요. 조건을 바꿔 다시 찾아볼 수 있어요.".into());
    } else {
        let mut lead = lead_sentence(data, goal, found);
        if data["args"]["earliest"] == true {
            lead.push_str(" 그중 마감이 가장 빠른 곳이에요.");
        }
        lines.push(lead);
        // The question's own words no listed row matches: said first, so the
        // field's rows never read as a match for them (PR #20 review H3).
        let unmatched = strings(data, "unmatched_words");
        if !unmatched.is_empty() {
            let quoted: Vec<String> = unmatched.iter().map(|w| format!("‘{w}’")).collect();
            lines.push(format!(
                "{}{} 직접 맞는 혜택은 찾지 못해 {} 분야 혜택을 보여 드려요.",
                and_list(&quoted),
                with_particle(unmatched.last().map_or("", String::as_str)),
                labels
            ));
        }
    }
    let asked_basis = shown
        .iter()
        .any(|(_, l)| l.contains("(질문하신 지역 기준)"));
    // What every shown row says is said once, after them (capture
    // 2026-11-04 #06 said "부산 거주 조건은 맞아요. 나머지 신청 자격은
    // 공고에서 확인해 주세요." under each of three rows).
    let blocks: Vec<&str> = shown.iter().map(|(_, block)| block.as_str()).collect();
    let (rows_shown, shared) = hoist_shared(&blocks);
    for (i, block) in rows_shown.iter().enumerate() {
        lines.push(format!("{}. {block}", i + 1));
    }
    // No parsed deadline: mazimi holds none, which says nothing about the
    // notice (PR #20 review M2). Said with the rows' shared sentences, once.
    let mut common: Vec<String> = Vec::new();
    if no_deadlines {
        common.push("마감일 정보가 없어요.".to_owned());
    }
    common.extend(shared);
    match shown.len() {
        _ if common.is_empty() => {}
        1 => lines.push(common.join(" ")),
        n => lines.push(format!("{n}개 모두 {}", common.join(" "))),
    }
    if let Some(saved) = data["saved_region_label"].as_str().filter(|_| asked_basis) {
        lines.push(format!("저장된 거주 지역은 {saved}{}.", copula(saved)));
    }
    let mentions = topic["mentions"].as_u64().unwrap_or(0);
    let shown_mentions = rows.len() - on_topic_rows.len();
    if topic.is_object() && mentions > 0 && shown_mentions > 0 {
        lines.push(format!(
            "다른 분야지만 설명에 {labels} 관련 내용이 있는 혜택 {shown_mentions}개도 목록 뒤에 함께 보여 드려요."
        ));
    }
    // Not final eligibility, unless every listed row already says what is
    // left to check (PR #20 review M1).
    let hedged = |l: &str| l.contains("확인해 주세요") || l.contains("확인이 필요해요");
    if !shown.is_empty() && !shown.iter().all(|(_, l)| hedged(l)) {
        lines.push(NOT_FINAL.into());
    }
    lines
}

/// A row block is "name · deadline" and, after `ROW_INDENT`, its sentences
/// joined by spaces (`row_line`).
const ROW_INDENT: &str = "\n   ";

/// A row block's name line and its sentences, each ending in '.'.
fn split_block(block: &str) -> (&str, Vec<String>) {
    let Some((name, detail)) = block.split_once(ROW_INDENT) else {
        return (block, Vec::new());
    };
    let parts: Vec<&str> = detail.split(". ").collect();
    let last = parts.len() - 1;
    let sentences = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i == last {
                (*p).to_owned()
            } else {
                format!("{p}.")
            }
        })
        .collect();
    (name, sentences)
}

/// The rows to print and the sentences every one of two or more rows says,
/// which are said once after them instead of under each.
fn hoist_shared(blocks: &[&str]) -> (Vec<String>, Vec<String>) {
    let split: Vec<(&str, Vec<String>)> = blocks.iter().map(|b| split_block(b)).collect();
    let shared: Vec<String> = match split.first() {
        // A sentence with a figure (date, D-day, amount) stays with its row,
        // where the grounding check reads it beside the row's name.
        Some((_, first)) if split.len() > 1 => first
            .iter()
            .filter(|s| !s.chars().any(|c| c.is_ascii_digit()))
            .filter(|s| split.iter().all(|(_, other)| other.contains(s)))
            .fold(Vec::new(), |mut out, s| {
                if !out.contains(s) {
                    out.push(s.clone());
                }
                out
            }),
        _ => Vec::new(),
    };
    let rows = split
        .into_iter()
        .map(|(name, sentences)| {
            let rest: Vec<String> = sentences
                .into_iter()
                .filter(|s| !shared.contains(s))
                .collect();
            if rest.is_empty() {
                name.to_owned()
            } else {
                format!("{name}{ROW_INDENT}{}", rest.join(" "))
            }
        })
        .collect();
    (rows, shared)
}

/// "부산 · 대학생 조건으로 찾은 주거 지원이 66개예요.": what the search ran
/// with and how many rows it kept, never who can get them (PR #20 review
/// M1: the rows are candidates, not eligibility).
fn lead_sentence(data: &Value, goal: &str, found: u64) -> String {
    let region = data["region_label"].as_str();
    let who = data["intent"]["enrollment_label"]
        .as_str()
        .or_else(|| who_label(goal));
    let basis: Vec<&str> = region.into_iter().chain(who).collect();
    let mut topic = strings(&data["topic"], "labels").join("·");
    if topic.is_empty() {
        topic = conditions(data, goal)
            .into_iter()
            .filter(|c| {
                Some(c.as_str()) != region && Some(c.as_str()) != who && !c.ends_with("마감")
            })
            .collect::<Vec<_>>()
            .join("·");
    }
    let topic = if topic.is_empty() {
        "혜택".to_owned()
    } else {
        format!("{topic} 지원")
    };
    let window = period_phrase(&data["args"])
        .map(|period| format!("{period}에 마감되는 "))
        .unwrap_or_default();
    let found_topic = format!("{window}{topic}");
    let subject = if basis.is_empty() {
        String::new()
    } else {
        format!("{} 조건으로 찾은 ", basis.join(" · "))
    };
    format!(
        "{subject}{found_topic}{} {found}개예요.",
        subject_particle(&found_topic)
    )
}

/// `과` after a final consonant, `와` after a vowel ("‘X’와 직접 …").
fn with_particle(word: &str) -> &'static str {
    if batchim(word) {
        "과"
    } else {
        "와"
    }
}

fn batchim(word: &str) -> bool {
    word.chars()
        .last()
        .filter(|c| ('\u{AC00}'..='\u{D7A3}').contains(c))
        .is_some_and(|c| !(c as u32 - 0xAC00).is_multiple_of(28))
}

fn subject_particle(word: &str) -> &'static str {
    if batchim(word) {
        "이"
    } else {
        "가"
    }
}

/// "A와 B", "A과 B" … joined as one phrase.
fn and_list(items: &[String]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(if batchim(&items[i - 1]) {
                "과 "
            } else {
                "와 "
            });
        }
        out.push_str(item);
    }
    out
}

/// A condition label in the user's words: the one table the answer reads
/// (users report 2026-10-31: "맞는 조건: 지역. 확인 필요: 필수 조건."). `None`:
/// an opaque bundle that names nothing by itself.
fn condition_phrase(label: &str, region: Option<&str>) -> Option<String> {
    const PHRASES: &[(&str, &str)] = &[
        ("연령", "나이"),
        ("나이", "나이"),
        ("시·군·구", "사는 구·군"),
        ("소득", "소득"),
        ("소득 구간", "소득"),
        ("학적", "학적"),
        ("학교", "학교"),
        ("신청 주체", "신청 대상"),
        ("무주택 여부", "무주택"),
        ("가구원 수", "가구원 수"),
        ("취업 상태", "취업 상태"),
    ];
    match label {
        "필수 조건" | "공고 조건" => None,
        "지역" | "거주 지역" => {
            Some(region.map_or_else(|| "거주 지역".to_owned(), |r| format!("{r} 거주")))
        }
        other => Some(
            PHRASES
                .iter()
                .find(|(l, _)| *l == other)
                .map_or(other, |(_, p)| *p)
                .to_owned(),
        ),
    }
}

/// `이에요` after a final consonant, `예요` after a vowel ("부산이에요",
/// "대구예요").
fn copula(word: &str) -> &'static str {
    let batchim = word
        .chars()
        .last()
        .filter(|c| ('\u{AC00}'..='\u{D7A3}').contains(c))
        .is_some_and(|c| !(c as u32 - 0xAC00).is_multiple_of(28));
    if batchim {
        "이에요"
    } else {
        "예요"
    }
}

/// One numbered row of a search answer, or `None` when its name, date,
/// D-day or amount is not this row's (or its check's).
/// `asked_region`: the region the question named. `basis`: the user has not
/// saved it (the search's `region_basis`): the row is said to be that
/// region's as the question's basis, never as met.
fn row_line(
    row: &Value,
    check: Option<&ToolResult>,
    asked_region: Option<&str>,
    search_region: Option<&str>,
    basis: bool,
) -> Option<String> {
    let region = asked_region.or(search_region);
    let basis_sentence =
        |region: &str| format!("지역은 {region} 거주 대상이에요(질문하신 지역 기준).");
    let mut asked = None;
    let mut opaque = false;
    let (met, unmet, unknown, notes) = match check {
        Some(check) => {
            let (met, mut unmet, mut unknown) = checked(check);
            // The question named the region (대구) and the check against the
            // saved profile failed it: the row is that region's, which is
            // said as the question's basis, never as a met condition
            // (re-review 4 H2).
            if let Some(region) = asked_region {
                let is_region = |l: &String| l == "지역" || l == "거주 지역";
                if unmet.iter().any(is_region) || (basis && unknown.iter().any(is_region)) {
                    unmet.retain(|l| !is_region(l));
                    unknown.retain(|l| !is_region(l));
                    asked = Some(basis_sentence(region));
                }
            }
            // An unknown bundle: what it lacks from the profile, else the
            // notice ("나머지 신청 자격은 공고에서").
            opaque = check.data["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| c["bundle"] == true && c["status"] != "pass" && c["status"] != "fail");
            for field in strings(&check.data, "missing_fields") {
                if let Some(words) = field_words(&field).map(str::to_owned) {
                    if !unknown.contains(&words) {
                        unknown.push(words);
                    }
                }
            }
            (met, unmet, unknown, Vec::new())
        }
        None => {
            if let (Some(region), true, true) =
                (asked_region, basis, row["local"].as_bool() == Some(true))
            {
                asked = Some(basis_sentence(region));
            }
            // The list's own reasons are said as they are ("부산 거주").
            (
                strings(row, "fit_reasons"),
                Vec::new(),
                Vec::new(),
                strings(row, "to_check"),
            )
        }
    };
    let phrases = |labels: &[String]| -> (Vec<String>, bool) {
        let mut out: Vec<String> = Vec::new();
        let mut opaque = false;
        for label in labels {
            match condition_phrase(label, region) {
                Some(p) if !out.contains(&p) => out.push(p),
                Some(_) => {}
                None => opaque = true,
            }
        }
        (out, opaque)
    };
    let (met, _) = phrases(&met);
    let (unmet, _) = phrases(&unmet);
    let (unknown, unknown_opaque) = phrases(&unknown);
    let mut sentences: Vec<String> = Vec::new();
    if !met.is_empty() {
        sentences.push(format!("{} 조건은 맞아요.", and_list(&met)));
    }
    if !unmet.is_empty() {
        sentences.push(format!("{} 조건은 맞지 않아요.", and_list(&unmet)));
    }
    if !unknown.is_empty() {
        sentences.push(format!("{} 조건은 확인이 필요해요.", and_list(&unknown)));
    }
    sentences.extend(
        notes
            .into_iter()
            .map(|n| if n.ends_with('.') { n } else { format!("{n}.") }),
    );
    // A row of another region (a reviewed exception) says so, never under
    // the search's region as its own (QA B11).
    let outside = strings(row, "outside_region");
    if !outside.is_empty() {
        sentences.push(format!(
            "{} 지역 대상이라 지역 예외에 해당하는지 확인해 주세요.",
            outside.join("·")
        ));
    }
    // The question bounds the amount and the row stores none (audit M6).
    if row["amount_unregistered"] == true {
        sentences.push("금액은 등록돼 있지 않아요.".into());
    }
    sentences.extend(asked);
    if opaque || unknown_opaque {
        sentences.push("나머지 신청 자격은 공고에서 확인해 주세요.".into());
    }
    // The block: the name, its deadline when it has one (a shared "no
    // deadline" is said once after the list), then the sentences. A row
    // with no title cannot be named.
    let name = row["title_display"]
        .as_str()
        .or_else(|| row["title"].as_str())?;
    let deadline = if row["deadline"].is_null() {
        String::new()
    } else {
        format!(" · {}", deadline_phrase(row))
    };
    let detail = sentences.join(" ");
    let block = if detail.is_empty() {
        format!("{name}{deadline}")
    } else {
        format!("{name}{deadline}{ROW_INDENT}{detail}")
    };
    let own: Vec<&Value> = std::iter::once(row).chain(check.map(|c| &c.data)).collect();
    ungrounded_with(&block, &facts_of(&own))
        .is_empty()
        .then_some(block)
}

/// A profile field in plain words ("소득 구간"): the one wording the answer's
/// rows and its profile question use.
pub(crate) fn field_words(field: &str) -> Option<&'static str> {
    Some(match field {
        "income_median_band" | "income_bracket" | "kosaf_support_bracket" | "income_median_pct" => {
            "소득 구간"
        }
        "homeless_household" | "is_homeless" => "무주택 여부",
        "household_size" => "가구원 수",
        "enrollment_status" => "학적",
        "employment_status" => "취업 상태",
        _ => return None,
    })
}

/// Programmes a search answer names one by one (the on-topic rows of its
/// last successful search, first [`NAMED`]).
pub fn named_ids(searches: &[&ToolResult]) -> Vec<Uuid> {
    let Some(last) = searches.iter().rev().find(|r| r.ok) else {
        return Vec::new();
    };
    named_ids_of(&last.data)
}

/// [`named_ids`] of one search result's data.
pub fn named_ids_of(data: &Value) -> Vec<Uuid> {
    shown_rows(data, &HashMap::new())
        .into_iter()
        .filter_map(|(row, _)| row["program_id"].as_str()?.parse().ok())
        .collect()
}

/// The rows a search answer shows, with their lines (unnumbered): on topic
/// (never a summary-only mention), each line grounded in its own row and
/// check, filtered before the first [`NAMED`] are taken. The one source of
/// both the answer's numbered rows and `found` (re-review 3 L1).
pub fn shown_rows<'a>(
    data: &'a Value,
    checks: &HashMap<Uuid, &ToolResult>,
) -> Vec<(&'a Value, String)> {
    let topical = data["topic"].is_object();
    let asked_region = data["intent"]["region"]
        .as_str()
        .and(data["region_label"].as_str());
    let basis = data["region_basis"].as_bool() == Some(true);
    data["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !topical || r["topic_match"].as_str() != Some("mentions"))
        .filter_map(|row| {
            let id: Option<Uuid> = row["program_id"].as_str().and_then(|s| s.parse().ok());
            let check = id.and_then(|id| checks.get(&id)).filter(|r| r.ok).copied();
            match row_line(
                row,
                check,
                asked_region,
                data["region_label"].as_str(),
                basis,
            ) {
                Some(line) => Some((row, line)),
                None => {
                    tracing::warn!("a search answer row failed its grounding check");
                    None
                }
            }
        })
        .take(NAMED)
        .collect()
}

/// One targeted question for the profile field that most often blocked a
/// check in this run, as an action. Only fields the user can answer.
pub fn ask_user(results: &[ToolResult]) -> Option<SuggestedAction> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for r in results
        .iter()
        .filter(|r| r.ok && r.tool == ToolName::CheckEligibility)
    {
        for field in strings(&r.data, "missing_fields") {
            if !mazimi_core::rule_engine::profile_answers(&field) {
                continue;
            }
            match counts.iter_mut().find(|(f, _)| *f == field) {
                Some((_, n)) => *n += 1,
                None => counts.push((field, 1)),
            }
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let (field, _) = counts.into_iter().next()?;
    let words = field_words(&field)?;
    let question = match words {
        "소득 구간" => "소득 구간을 알려 주시면 소득 조건을 확인할 수 있어요.",
        "무주택 여부" => "무주택 여부를 알려 주시면 주거 혜택의 주택 조건을 확인할 수 있어요.",
        "가구원 수" => "가구원 수를 알려 주시면 가구 조건을 확인할 수 있어요.",
        "학적" => "재학·휴학·졸업 중 어디인지 알려 주시면 학적 조건을 확인할 수 있어요.",
        _ => "취업 상태를 알려 주시면 고용 조건을 확인할 수 있어요.",
    };
    Some(SuggestedAction {
        kind: "ask_user".into(),
        label: format!("{words} 알려주기"),
        payload: json!({"field": field, "question": question}),
    })
}

/// Next steps after a search: alerts for what was found (a new run, with
/// the programmes as the selection; every alert is a confirmation card),
/// the checklist of the first one, and the nearest field when the asked one
/// had nothing.
pub fn next_actions(results: &[ToolResult]) -> Vec<SuggestedAction> {
    let mut actions = Vec::new();
    let Some(search) = results
        .iter()
        .rev()
        .find(|r| r.ok && r.tool == ToolName::SearchBenefits)
    else {
        return actions;
    };
    let rows: Vec<&Value> = search.data["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["topic_match"].as_str() != Some("mentions"))
        .collect();
    let alerted = results.iter().any(|r| r.tool == ToolName::SetAlert);
    let with_deadline: Vec<&str> = rows
        .iter()
        .filter(|r| r["deadline"].is_string())
        .filter_map(|r| r["program_id"].as_str())
        .take(NAMED)
        .collect();
    if !alerted && !with_deadline.is_empty() {
        actions.push(SuggestedAction {
            kind: "goal".into(),
            label: format!("{}개 마감 알림 켜기", with_deadline.len()),
            payload: json!({"goal": SELECTED_ALERT_GOAL, "goal_kind": "enable_alerts", "program_ids": with_deadline}),
        });
    }
    let prepared = results.iter().any(|r| r.tool == ToolName::GetPreparation);
    if let (Some(first), false) = (rows.first(), prepared) {
        if let Some(id) = first["program_id"].as_str() {
            actions.push(SuggestedAction {
                kind: "pick_program".into(),
                label: "신청 준비 체크리스트 만들기".into(),
                payload: json!({"program_id": id, "goal": PREPARE_GOAL, "goal_kind": "prepare_program"}),
            });
        }
    }
    if rows.is_empty() {
        if let (Some(field), Some(label)) = (
            search.data["topic"]["nearest"]["field"].as_str(),
            search.data["topic"]["nearest"]["label"].as_str(),
        ) {
            actions.push(SuggestedAction {
                kind: "goal".into(),
                label: format!("{label} 혜택 보기"),
                payload: json!({"goal": format!("{label} 분야 혜택 찾아줘"), "goal_kind": format!("search_{field}")}),
            });
        }
    }
    actions
}

/// After a 신청 준비 run with no alert step: "마감 알림 켜기" as a button
/// (tapping it asks for the alert, which then becomes a card).
pub fn alert_offer(results: &[ToolResult]) -> Option<SuggestedAction> {
    let prepared = results
        .iter()
        .any(|r| r.ok && r.tool == ToolName::GetPreparation && r.program_id.is_some());
    let alerted = results.iter().any(|r| r.tool == ToolName::SetAlert);
    (prepared && !alerted).then(|| SuggestedAction {
        kind: "goal".into(),
        label: "마감 알림 켜기".into(),
        payload: json!({"goal": PROGRAM_ALERT_GOAL, "goal_kind": "enable_alerts"}),
    })
}

/// The goal behind the 신청 준비 alert button (the programme on screen).
pub const PROGRAM_ALERT_GOAL: &str = "이 혜택 마감 알림 켜줘";

/// The goal behind "N개 마감 알림 켜기": alerts for the selection it sends.
pub const SELECTED_ALERT_GOAL: &str = "고른 혜택 마감 알림 켜줘";
/// The goal behind "신청 준비 체크리스트 만들기" (with that programme).
/// A question: the checklist and the vault match, no handoff cards (security
/// review L1: the chip promises a checklist).
pub const PREPARE_GOAL: &str = "이 혜택 신청하려면 뭐 준비해야 돼?";

static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new("‘([^’]+)’").unwrap());
static DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{1,2})월 (\d{1,2})일").unwrap());
static DDAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"D-(\d+)").unwrap());
static AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d[\d,]*)\s?(만\s?원|원)").unwrap());

/// What this run's results allow a line to state.
pub struct Facts {
    strings: HashSet<String>,
    dates: HashSet<String>,
    days: HashSet<i64>,
    amounts: HashSet<i64>,
}

fn collect(value: &Value, key: &str, facts: &mut Facts) {
    match value {
        Value::String(s) => {
            facts.strings.insert(s.clone());
            facts
                .strings
                .insert(mazimi_core::title_display::title_display(s));
            if let Some(date) = month_day(s) {
                facts.dates.insert(date);
            }
        }
        Value::Number(n) => {
            if let Some(n) = n.as_i64() {
                if key == "deadline_days" {
                    facts.days.insert(n);
                }
                if key.contains("amount") || key == "monthly" || key == "semester" || key == "once"
                {
                    facts.amounts.insert(n);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, key, facts)),
        Value::Object(map) => map.iter().for_each(|(k, v)| collect(v, k, facts)),
        _ => {}
    }
}

/// Facts of some result values (one row and its check).
pub fn facts_of(values: &[&Value]) -> Facts {
    let mut facts = Facts {
        strings: HashSet::new(),
        dates: HashSet::new(),
        days: HashSet::new(),
        amounts: HashSet::new(),
    };
    for value in values {
        collect(value, "", &mut facts);
    }
    facts
}

pub fn facts(results: &[ToolResult]) -> Facts {
    let mut facts = Facts {
        strings: HashSet::new(),
        dates: HashSet::new(),
        days: HashSet::new(),
        amounts: HashSet::new(),
    };
    for r in results {
        collect(&r.data, "", &mut facts);
        if let Some(e) = &r.error {
            facts.strings.insert(e.message.clone());
        }
    }
    facts
}

/// The facts in `line` that no result of this run carries: quoted names,
/// `M월 D일` dates, `D-n` and amounts.
pub fn ungrounded(line: &str, results: &[ToolResult]) -> Vec<String> {
    ungrounded_with(line, &facts(results))
}

/// [`ungrounded`] against facts collected once. A quoted name must be a
/// result string. Dates, D-days and amounts are checked in what is left
/// after every result string the line quotes word for word is taken out:
/// a title or condition that itself says "월 20만원" or "11월 1일" is
/// grounded as a whole.
pub fn ungrounded_with(line: &str, facts: &Facts) -> Vec<String> {
    let mut missing = Vec::new();
    for c in QUOTED.captures_iter(line) {
        let quoted = &c[1];
        if !facts.strings.contains(quoted) {
            missing.push(format!("‘{quoted}’"));
        }
    }
    let mut copied: Vec<&String> = facts
        .strings
        .iter()
        .filter(|s| s.chars().count() >= 2 && line.contains(s.as_str()))
        .collect();
    copied.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut rest = line.to_owned();
    for s in copied {
        rest = rest.replace(s.as_str(), " ");
    }
    let line = rest.as_str();
    for c in DATE.captures_iter(line) {
        if !facts.dates.contains(&c[0]) {
            missing.push(c[0].to_owned());
        }
    }
    for c in DDAY.captures_iter(line) {
        if !c[1].parse().is_ok_and(|d: i64| facts.days.contains(&d)) {
            missing.push(c[0].to_owned());
        }
    }
    for c in AMOUNT.captures_iter(line) {
        let digits: String = c[1].chars().filter(char::is_ascii_digit).collect();
        let Ok(n) = digits.parse::<i64>() else {
            continue;
        };
        let won = if c[2].starts_with('만') {
            n * 10_000
        } else {
            n
        };
        if !facts.amounts.contains(&won) {
            missing.push(c[0].to_owned());
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::StepStatus;
    use chrono::Utc;
    use mazimi_core::agent::Permission;

    fn result(tool: ToolName, data: Value) -> ToolResult {
        ToolResult {
            seq: 0,
            tool,
            permission: Permission::Read,
            program_id: data["program_id"].as_str().and_then(|s| s.parse().ok()),
            ok: true,
            status: StepStatus::Ok,
            data,
            evidence: Vec::new(),
            error: None,
            executed_at: Utc::now(),
        }
    }

    fn search(rows: Value, topic: Value) -> ToolResult {
        result(
            ToolName::SearchBenefits,
            json!({"results": rows, "topic": topic, "region_label": "부산",
                "intent": {}, "args": {"fields": ["housing"]}, "total_candidates": 2}),
        )
    }

    /// Round 2 (code L-9): a length or an implicit window is said as a
    /// length; dates only for a named calendar period.
    #[test]
    fn a_window_is_said_the_way_the_goal_said_it() {
        let data = |to: u16, from: Option<u16>| json!({"args": {"deadline_within_days": to, "deadline_from_days": from}});
        let parts = conditions(&data(14, None), "마감 임박한 장학금");
        assert!(parts.contains(&"14일 안 마감".to_owned()), "{parts:?}");
        let parts = conditions(&data(3, None), "3일 안에 마감되는 장학금");
        assert!(parts.contains(&"3일 안 마감".to_owned()), "{parts:?}");
        let parts = conditions(&data(30, Some(10)), "다음 달 마감 장학금");
        assert!(parts.iter().any(|p| p.contains('~')), "{parts:?}");
        // Round 3 (code L-3): a window starting after today is dates, and
        // the reading travels with the arguments, not the goal re-read.
        let parts = conditions(&data(30, Some(10)), "장학금 알려줘");
        assert!(parts.iter().any(|p| p.contains('~')), "{parts:?}");
        let named = json!({"args": {"deadline_within_days": 5, "deadline_named": true}});
        let parts = conditions(&named, "장학금 알려줘");
        assert!(parts.iter().any(|p| p.contains('월')), "{parts:?}");
    }

    #[test]
    fn reasons_become_short_condition_names() {
        assert_eq!(
            fit_reasons(&[
                "부산 거주 조건에 맞아요.".into(),
                "현재 연령 조건에 맞아요.".into(),
                "마감이 5일 남았어요! 서두르세요.".into(),
                "소득 구간 3 조건 맞음".into(),
                "무주택 조건 맞음".into(),
            ]),
            ["부산 거주", "나이", "소득 구간", "무주택"]
        );
    }

    /// User report (2026-10-31): the answer was one dense paragraph with
    /// "맞는 조건: 지역. 확인 필요: 필수 조건." per row and the disclaimer
    /// twice. Now: the count, then one line per row with its name, deadline
    /// and only what is specifically left to check; no disclaimer in the body
    /// (the warning says it once).
    #[test]
    fn the_answer_names_the_topic_the_top_rows_and_what_is_left() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let rows = json!([
            {"program_id": a, "title": "(변경·중지·연장 등 )부산광역시 평생함께 청년모두가 주거비 지원",
             "title_display": "부산광역시 평생함께 청년모두가 주거비 지원", "deadline": "2026-11-30",
             "deadline_days": 38, "topic_match": "both", "fit_reasons": ["부산 거주", "나이"],
             "remaining_checks": ["주택·임대차 조건 확인이 필요해요", "공고의 자격 조건 검토가 필요해요"],
             "to_check": ["주택·임대차 조건 확인이 필요해요"]},
            {"program_id": b, "title": "기초과학자장학생", "deadline": null, "topic_match": "mentions"}
        ]);
        let topic = json!({"labels": ["주거"], "on_topic": 1, "mentions": 1});
        let s = search(rows, topic);
        let lines = search_lines(&[&s], &HashMap::new(), "부산 사는 대학생인데 월세 지원");
        assert_eq!(lines[0], "부산 · 대학생 조건으로 찾은 주거 지원이 1개예요.");
        assert_eq!(
            lines[1],
            "1. 부산광역시 평생함께 청년모두가 주거비 지원 · 마감 11월 30일(D-38)\n   부산 거주와 나이 조건은 맞아요. 주택·임대차 조건 확인이 필요해요."
        );
        let answer = lines.join("\n");
        assert!(
            !answer.contains("기초과학자장학생"),
            "a mention is not named: {answer}"
        );
        assert!(
            answer.contains("설명에 주거 관련 내용이 있는 혜택 1개"),
            "{answer}"
        );
        assert!(
            !answer.contains(NOT_FINAL),
            "the row already says what to check: {answer}"
        );
        assert!(!answer.contains("자격 확정"), "{answer}");
        assert!(
            !answer.contains("검토가 필요해요\n") && !answer.contains("공고의 자격 조건"),
            "{answer}"
        );
        assert!(!answer.contains("받을 수 있어요") && !answer.contains("신청 가능"));
        for line in &lines {
            assert!(
                ungrounded(line, std::slice::from_ref(&s)).is_empty(),
                "{line}"
            );
        }
        // An eligibility check of the row: an unknown bundle says nothing by
        // itself; the profile fact it lacked is what is left to check.
        let check = result(
            ToolName::CheckEligibility,
            json!({"program_id": a, "missing_fields": ["income_median_band"], "checks": [
                {"label": "지역", "status": "pass"}, {"label": "나이", "status": "pass"},
                {"label": "필수 조건", "status": "unknown", "bundle": true}]}),
        );
        let checks: HashMap<Uuid, &ToolResult> = [(a, &check)].into_iter().collect();
        let answer = search_lines(&[&s], &checks, "").join("\n");
        assert!(
            answer.contains("부산광역시 평생함께 청년모두가 주거비 지원 · 마감 11월 30일(D-38)\n   부산 거주와 나이 조건은 맞아요. 소득 조건은 확인이 필요해요. 나머지 신청 자격은 공고에서 확인해 주세요."),
            "{answer}"
        );
        assert!(
            !answer.contains("필수 조건") && !answer.contains("확인 필요:"),
            "{answer}"
        );
        // Nothing specific left: the row is its name and deadline.
        let bare = result(
            ToolName::CheckEligibility,
            json!({"program_id": a, "checks": [{"label": "필수 조건", "status": "unknown", "bundle": true}]}),
        );
        let checks: HashMap<Uuid, &ToolResult> = [(a, &bare)].into_iter().collect();
        let answer = search_lines(&[&s], &checks, "").join("\n");
        assert!(
            answer.contains(
                "1. 부산광역시 평생함께 청년모두가 주거비 지원 · 마감 11월 30일(D-38)\n   나머지 신청 자격은 공고에서 확인해 주세요."
            ),
            "{answer}"
        );
    }

    /// Capture 2026-11-04 #06: what every listed row says is said once after
    /// the list, not under each row; a row left with nothing of its own is
    /// its name.
    #[test]
    fn a_sentence_every_row_says_is_said_once() {
        assert_eq!(
            hoist_shared(&[
                "가 · 마감 1월 1일\n   부산 거주 조건은 맞아요. 나머지는 공고에서.",
                "나\n   부산 거주 조건은 맞아요. 소득 조건은 확인이 필요해요. 나머지는 공고에서."
            ]),
            (
                vec![
                    "가 · 마감 1월 1일".to_owned(),
                    "나\n   소득 조건은 확인이 필요해요.".to_owned()
                ],
                vec![
                    "부산 거주 조건은 맞아요.".to_owned(),
                    "나머지는 공고에서.".to_owned()
                ]
            )
        );
        // A figure stays with its row; a sentence said twice is hoisted once.
        assert_eq!(
            hoist_shared(&[
                "가\n   마감 1월 1일이에요. 확인해요. 확인해요.",
                "나\n   마감 1월 1일이에요. 확인해요."
            ])
            .1,
            vec!["확인해요.".to_owned()]
        );
        // One row, or rows that share nothing: unchanged.
        let one = ["가\n   부산 거주 조건은 맞아요."];
        assert_eq!(hoist_shared(&one), (vec![one[0].to_owned()], Vec::new()));
        let apart = [
            "가\n   나이 조건은 맞아요.",
            "나",
            "다\n   나이 조건은 맞아요.",
        ];
        assert_eq!(hoist_shared(&apart).1, Vec::<String>::new());
    }

    /// Rows with deadlines that share a sentence: it is said once after them,
    /// with no 마감일 line, and each row keeps its own deadline.
    #[test]
    fn rows_with_deadlines_say_their_shared_sentence_once() {
        let rows = json!([
            {"program_id": Uuid::new_v4(), "title": "청년 월세 지원", "deadline": "2026-11-30",
             "deadline_days": 38, "topic_match": "both", "fit_reasons": ["부산 거주"]},
            {"program_id": Uuid::new_v4(), "title": "청년 전세 지원", "deadline": "2026-12-10",
             "deadline_days": 48, "topic_match": "both", "fit_reasons": ["부산 거주"]}
        ]);
        let s = search(
            rows,
            json!({"labels": ["주거"], "on_topic": 2, "mentions": 0}),
        );
        let lines = search_lines(&[&s], &HashMap::new(), "");
        assert_eq!(lines[1], "1. 청년 월세 지원 · 마감 11월 30일(D-38)");
        assert_eq!(lines[2], "2. 청년 전세 지원 · 마감 12월 10일(D-48)");
        assert_eq!(lines[3], "2개 모두 부산 거주 조건은 맞아요.");
        let answer = lines.join("\n");
        assert_eq!(
            answer.matches("부산 거주 조건은 맞아요").count(),
            1,
            "{answer}"
        );
        assert!(!answer.contains("마감일 정보"), "{answer}");
        for line in &lines {
            assert!(
                ungrounded(line, std::slice::from_ref(&s)).is_empty(),
                "{line}"
            );
        }
    }

    /// PR #20 review H3, H2, M1, M2: a field search says first which of the
    /// question's words nothing matched; the earliest answer says it is the
    /// one closing first; the lead counts candidates, never eligibility; no
    /// deadline is "정보가 없어요", not a claim about the notice.
    #[test]
    fn the_lead_says_what_was_and_was_not_found() {
        let a = Uuid::new_v4();
        let rows = json!([{"program_id": a, "title": "청년 월세 지원", "topic_match": "field"}]);
        let mut s = search(
            rows,
            json!({"labels": ["주거"], "on_topic": 12, "mentions": 0}),
        );
        s.data["unmatched_words"] = json!(["전세사기"]);
        let lines = search_lines(&[&s], &HashMap::new(), "");
        assert_eq!(lines[0], "부산 조건으로 찾은 주거 지원이 12개예요.");
        assert_eq!(
            lines[1],
            "‘전세사기’와 직접 맞는 혜택은 찾지 못해 주거 분야 혜택을 보여 드려요."
        );
        let answer = lines.join("\n");
        assert!(
            !answer.contains("받을 수 있") && !answer.contains("잘 맞는"),
            "{answer}"
        );
        assert!(answer.contains("마감일 정보가 없어요."), "{answer}");
        // A row with nothing left to check keeps the not-final line.
        assert!(answer.contains(NOT_FINAL), "{answer}");
        s.data["unmatched_words"] = json!([]);
        s.data["args"]["earliest"] = json!(true);
        let lines = search_lines(&[&s], &HashMap::new(), "");
        assert_eq!(
            lines[0],
            "부산 조건으로 찾은 주거 지원이 12개예요. 그중 마감이 가장 빠른 곳이에요."
        );
    }

    #[test]
    fn nothing_on_topic_says_so_and_offers_the_nearest_field() {
        let s = search(
            json!([]),
            json!({"labels": ["주거"], "on_topic": 0, "mentions": 0,
                "nearest": {"field": "finance", "label": "금융", "count": 4}}),
        );
        let answer = search_lines(&[&s], &HashMap::new(), "").join(" ");
        assert!(answer.contains("주거 분야에서 지금 조건에 맞는 혜택을 찾지 못했어요."));
        assert!(answer.contains("가까운 분야인 금융에는 같은 조건의 혜택이 4개 있어요."));
        let actions = next_actions(&[s]);
        assert_eq!(actions[0].label, "금융 혜택 보기");
        assert_eq!(actions[0].payload["goal"], "금융 분야 혜택 찾아줘");
    }

    #[test]
    fn next_actions_offer_alerts_and_a_checklist() {
        let a = Uuid::new_v4();
        let s = search(
            json!([{"program_id": a, "title": "t", "deadline": "2026-11-30", "topic_match": "field"}]),
            json!({"labels": ["주거"], "on_topic": 1, "mentions": 0}),
        );
        let actions = next_actions(std::slice::from_ref(&s));
        assert_eq!(actions[0].label, "1개 마감 알림 켜기");
        assert_eq!(actions[0].payload["program_ids"], json!([a]));
        assert_eq!(actions[1].label, "신청 준비 체크리스트 만들기");
        assert_eq!(actions[1].payload["program_id"], json!(a));
    }

    /// The grounding check fails on a fact no result carries.
    #[test]
    fn an_ungrounded_fact_is_caught() {
        let a = Uuid::new_v4();
        let s = search(
            json!([{"program_id": a, "title": "청년 월세 지원", "deadline": "2026-11-30",
                "deadline_days": 38, "benefit_amount_monthly": 200000}]),
            json!({"labels": ["주거"], "on_topic": 1, "mentions": 0}),
        );
        let results = [s];
        assert!(ungrounded(
            "‘청년 월세 지원’ — 마감 11월 30일(D-38), 월 200,000원.",
            &results
        )
        .is_empty());
        assert!(ungrounded("월 20만 원", &results).is_empty());
        assert_eq!(
            ungrounded("‘청년 전세 지원’도 있어요.", &results),
            ["‘청년 전세 지원’"]
        );
        assert_eq!(ungrounded("마감 12월 1일", &results), ["12월 1일"]);
        assert_eq!(ungrounded("D-3 남았어요", &results), ["D-3"]);
        assert_eq!(ungrounded("월 50만 원을 줘요", &results), ["50만 원"]);
        // Code review H1: an amount or a date inside a quoted title or a
        // copied condition is that string's, not a new fact.
        let titled = search(
            json!([{"program_id": a, "title": "청년 월세 월 20만원 지원(11월 1일 접수)",
                "fit_reasons": ["월 소득 200만원 이하"], "deadline": null}]),
            json!({"labels": ["주거"], "on_topic": 1, "mentions": 0}),
        );
        let titled = [titled];
        assert!(ungrounded(
            "1. ‘청년 월세 월 20만원 지원(11월 1일 접수)’ — 마감일 정보 없음. 맞는 조건: 월 소득 200만원 이하.",
            &titled
        )
        .is_empty());
        assert_eq!(ungrounded("월 30만원을 줘요", &titled), ["30만원"]);
    }

    /// Code review M1: `found` is the rows the answer names: on topic, the
    /// first three, never a summary-only mention.
    #[test]
    fn found_is_what_the_answer_names() {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let data = json!({"topic": {"on_topic": 4}, "results": [
            {"program_id": ids[0], "title": "가", "topic_match": "both"},
            {"program_id": ids[1], "title": "나", "topic_match": "mentions"},
            {"program_id": ids[2], "title": "다", "topic_match": "field"},
            {"program_id": ids[3], "title": "라", "topic_match": "title"},
            {"program_id": ids[4], "title": "마", "topic_match": "field"},
        ]});
        assert_eq!(named_ids_of(&data), [ids[0], ids[2], ids[3]]);
        let plain = json!({"results": [{"program_id": ids[1], "title": "나"}]});
        assert_eq!(named_ids_of(&plain), [ids[1]]);
        // Re-review L3: a row the answer cannot name (no title to quote) is
        // not found either.
        let untitled =
            json!({"results": [{"program_id": ids[1]}, {"program_id": ids[2], "title": "다"}]});
        assert_eq!(named_ids_of(&untitled), [ids[2]]);
    }

    /// Code review L1: a row's line may not carry another row's deadline.
    #[test]
    fn a_line_is_grounded_in_its_own_row() {
        let a = Uuid::new_v4();
        let row_a = json!({"program_id": a, "title": "A 지원", "deadline": "2026-11-30"});
        assert!(ungrounded_with("1. ‘A 지원’ — 마감 11월 30일.", &facts_of(&[&row_a])).is_empty());
        assert_eq!(
            ungrounded_with("1. ‘A 지원’ — 마감 12월 15일.", &facts_of(&[&row_a])),
            ["12월 15일"]
        );
    }

    /// A region the question names: a failed region check is said as the
    /// question's basis, never as a met condition (re-review 4 H2).
    #[test]
    fn the_asked_region_is_the_one_the_line_reports() {
        let a = Uuid::new_v4();
        let rows = json!([{"program_id": a, "title": "대구 청년 월세", "topic_match": "both"}]);
        let mut s = search(
            rows,
            json!({"labels": ["주거"], "on_topic": 1, "mentions": 0}),
        );
        s.data["intent"] = json!({"region": "daegu"});
        s.data["region_label"] = json!("대구");
        let check = result(
            ToolName::CheckEligibility,
            json!({"program_id": a, "checks": [
                {"label": "지역", "status": "fail"}, {"label": "나이", "status": "pass"}]}),
        );
        let checks: HashMap<Uuid, &ToolResult> = [(a, &check)].into_iter().collect();
        let answer = search_lines(&[&s], &checks, "").join(" ");
        assert!(
            answer.contains("지역은 대구 거주 대상이에요(질문하신 지역 기준)."),
            "{answer}"
        );
        assert!(!answer.contains("지역(대구)"), "{answer}");
        assert!(!answer.contains("안 맞는 조건"), "{answer}");
        // No saved region of another sido: nothing more said.
        assert!(!answer.contains("저장된 거주 지역"), "{answer}");
        s.data["saved_region_label"] = json!("부산");
        let answer = search_lines(&[&s], &checks, "").join(" ");
        assert!(
            answer.contains("저장된 거주 지역은 부산이에요."),
            "{answer}"
        );
        // A region check the server passed stays a met condition.
        let passed = result(
            ToolName::CheckEligibility,
            json!({"program_id": a, "checks": [
                {"label": "지역", "status": "pass"}, {"label": "나이", "status": "pass"}]}),
        );
        let checks: HashMap<Uuid, &ToolResult> = [(a, &passed)].into_iter().collect();
        let answer = search_lines(&[&s], &checks, "").join(" ");
        assert!(
            answer.contains("1. 대구 청년 월세\n   대구 거주와 나이 조건은 맞아요."),
            "{answer}"
        );
        assert!(!answer.contains("질문하신 지역 기준"), "{answer}");
        assert!(!answer.contains("저장된 거주 지역"), "{answer}");
        assert_eq!(copula("대구"), "예요");
    }

    /// Re-review 5 H2: with no check, a region the user has not saved is
    /// the question's basis on a row of that region's own, never 맞는 조건
    /// (the search drops the residence reason at the source).
    #[test]
    fn an_unsaved_asked_region_is_the_basis_without_a_check_too() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let rows = json!([
            {"program_id": a, "title": "대구 청년 월세", "topic_match": "both",
             "local": true, "fit_reasons": ["나이"]},
            {"program_id": b, "title": "청년 월세 한시 특별지원", "topic_match": "both",
             "fit_reasons": ["나이"]}
        ]);
        let mut s = search(
            rows,
            json!({"labels": ["주거"], "on_topic": 2, "mentions": 0}),
        );
        s.data["intent"] = json!({"region": "daegu"});
        s.data["region_label"] = json!("대구");
        s.data["region_basis"] = json!(true);
        s.data["saved_region_label"] = json!("부산");
        let lines = search_lines(&[&s], &HashMap::new(), "");
        let local = lines.iter().find(|l| l.contains("대구 청년 월세")).unwrap();
        assert!(
            local.contains("지역은 대구 거주 대상이에요(질문하신 지역 기준)."),
            "{local}"
        );
        assert!(
            !local.contains("대구 거주,") && !local.contains("대구 거주."),
            "{local}"
        );
        let nationwide = lines.iter().find(|l| l.contains("한시 특별지원")).unwrap();
        assert!(!nationwide.contains("질문하신 지역 기준"), "{nationwide}");
        assert!(
            lines.iter().any(|l| l == "저장된 거주 지역은 부산이에요."),
            "{lines:?}"
        );
        // The saved region is the asked one: no basis, as before.
        s.data["region_basis"] = json!(false);
        s.data["saved_region_label"] = Value::Null;
        let answer = search_lines(&[&s], &HashMap::new(), "").join(" ");
        assert!(!answer.contains("질문하신 지역 기준"), "{answer}");
    }

    /// Re-review 5 H2: no saved region, with a check: the region the check
    /// could not read is the basis, not 확인 필요, and no saved-region line.
    #[test]
    fn no_saved_region_is_the_basis_on_the_check_path() {
        let a = Uuid::new_v4();
        let rows = json!([{"program_id": a, "title": "대구 청년 월세", "topic_match": "both", "local": true}]);
        let mut s = search(
            rows,
            json!({"labels": ["주거"], "on_topic": 1, "mentions": 0}),
        );
        s.data["intent"] = json!({"region": "daegu"});
        s.data["region_label"] = json!("대구");
        s.data["region_basis"] = json!(true);
        let check = result(
            ToolName::CheckEligibility,
            json!({"program_id": a, "checks": [
                {"label": "지역", "status": "unknown"}, {"label": "나이", "status": "pass"}]}),
        );
        let checks: HashMap<Uuid, &ToolResult> = [(a, &check)].into_iter().collect();
        let answer = search_lines(&[&s], &checks, "").join(" ");
        assert!(
            answer.contains("지역은 대구 거주 대상이에요(질문하신 지역 기준)."),
            "{answer}"
        );
        assert!(!answer.contains("확인 필요: 지역"), "{answer}");
        assert!(!answer.contains("저장된 거주 지역"), "{answer}");
    }

    #[test]
    fn a_blocking_field_becomes_one_question() {
        let check = |fields: Value| {
            result(
                ToolName::CheckEligibility,
                json!({"missing_fields": fields}),
            )
        };
        let ask = ask_user(&[
            check(json!(["income_median_band"])),
            check(json!(["income_median_band", "household_size"])),
        ])
        .unwrap();
        assert_eq!(ask.kind, "ask_user");
        assert_eq!(ask.payload["field"], "income_median_band");
        assert!(ask_user(&[check(json!(["birth_year"]))]).is_none());
        // Review B5: a field no profile answers is never the question, even
        // when it is the most frequent one.
        let ask = ask_user(&[
            check(json!(["target_group", "household_size"])),
            check(json!(["target_group", "gender"])),
        ])
        .unwrap();
        assert_eq!(ask.payload["field"], "household_size");
    }
}
