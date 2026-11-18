//! Structured income conditions of a programme (v1 feedback F6.1).
//!
//! A condition exists only where a source field codes one. Free text is
//! read for one thing: the ceiling the notice states ([`stated_bounds`]),
//! which decides a coded run ([`decision_bound`]).
//! - 보조금24 `supportConditions` JA0201–JA0205: the accepted 기준 중위소득
//!   bands, stored by the worker as `normalized_payload.income_median_bands`
//!   (only when not every band is set).
//! - 온통청년 `earnCndSeCd` 0043002: annual income bounds in 만원
//!   (`youth_conditions.income_annual_min_manwon` / `_max_manwon`), and
//!   0043003: an income condition described in text
//!   (`youth_conditions.income_condition_text`).
//!
//! The check ([`check`]) compares the 보조금24 bound with the user's own
//! 기준 중위소득 band answer (`income_median_band`, one of
//! [`crate::rule_engine::INCOME_MEDIAN_BANDS`]):
//! - the flagged bands must run contiguously from 0 % ("0-50", "0-50 +
//!   51-75", …): that is a "중위소득 N % 이하" bound. Any other set (gaps,
//!   only high bands) is Unknown;
//! - Pass when the user's whole band is within N, Fail when it lies wholly
//!   above N, Unknown when it straddles N, when the answer is 모름
//!   (`unknown`) or missing.
//!
//! The 온통청년 conditions (연소득 in 만원, or text) are never comparable with
//! a band and stay Unknown.
//!
//! Known limit (prod, 2026-10-23): the 보조금24 flags are loose. 243 of the
//! 1,203 rows flagged "0-50 only" name no income in their text, and many mean
//! 수급자·차상위 status rather than a percentage. So [`check`] decides only
//! on a ceiling the notice text states ("중위소득 150% 이하") inside the coded
//! run ([`decision_bound`]); otherwise the coded bound is a named check,
//! never a hide and never a Pass (audit M1).
use serde_json::Value;

use crate::models::Program;
use crate::rule_engine::RuleVerdict;

/// What a programme's codes say about income.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomeCondition {
    /// 기준 중위소득 bands the provider flagged ("0-50", "51-75", …).
    MedianBands(Vec<String>),
    /// Annual income bounds in 만원.
    AnnualIncome { min: Option<i64>, max: Option<i64> },
    /// The source codes an income condition and describes it in text.
    Described,
}

/// The profile field that would answer an income question, for the
/// missing-check label ([`crate::condition_review::profile_label`]).
pub const PROFILE_FIELD: &str = "income_bracket";

/// The programme's structured income condition, if any.
pub fn condition(program: &Program) -> Option<IncomeCondition> {
    let payload = program.normalized_payload.as_ref()?;
    if let Some(bands) = payload
        .get("income_median_bands")
        .and_then(Value::as_array)
        .filter(|bands| !bands.is_empty())
    {
        return Some(IncomeCondition::MedianBands(
            bands
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ));
    }
    let youth = payload.get("youth_conditions")?;
    let amount = |key: &str| youth.get(key).and_then(Value::as_i64);
    let (min, max) = (
        amount("income_annual_min_manwon"),
        amount("income_annual_max_manwon"),
    );
    if min.is_some() || max.is_some() {
        return Some(IncomeCondition::AnnualIncome { min, max });
    }
    youth
        .get("income_condition_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|_| IncomeCondition::Described)
}

/// Every 기준 중위소득 band code a programme can flag (`programs.income_median_bands`).
pub const MEDIAN_BANDS: [&str; 5] = ["0-50", "51-75", "76-100", "101-200", "200+"];

/// The stored band code for `value`, or `None` when it is not one.
pub fn median_band(value: &str) -> Option<&'static str> {
    MEDIAN_BANDS
        .iter()
        .copied()
        .find(|band| *band == value.trim())
}

/// The bands of a "중위소득 N % 이하" run, in order, with the bound each
/// run up to it states.
const MEDIAN_RUN: [(&str, f64); 4] = [
    ("0-50", 50.0),
    ("51-75", 75.0),
    ("76-100", 100.0),
    ("101-200", 200.0),
];

/// The "중위소득 N % 이하" bound of 보조금24 bands: the flagged bands must
/// be a contiguous run from 0 %. `None` for any other set.
pub fn median_bound(bands: &[String]) -> Option<f64> {
    const RUN: [(&str, f64); 4] = MEDIAN_RUN;
    let mut flagged: Vec<&str> = bands.iter().map(String::as_str).collect();
    flagged.sort_by_key(|b| {
        RUN.iter()
            .position(|(code, _)| code == b)
            .unwrap_or(usize::MAX)
    });
    flagged.dedup();
    if flagged.is_empty() || flagged.len() > RUN.len() {
        return None;
    }
    flagged
        .iter()
        .zip(RUN.iter())
        .all(|(b, (code, _))| b == code)
        .then(|| RUN[flagged.len() - 1].1)
}

/// The income check for a user whose band answer is `band` (see the module
/// docs). Unanswered or 모름 is Unknown, never Pass.
pub fn verdict(condition: &IncomeCondition, band: Option<&str>) -> RuleVerdict {
    let IncomeCondition::MedianBands(bands) = condition else {
        return RuleVerdict::Unknown;
    };
    let Some(bound) = median_bound(bands) else {
        return RuleVerdict::Unknown;
    };
    band_verdict(bound, band)
}

/// The user's band against a "중위소득 `bound` % 이하" ceiling: Pass when the
/// whole band is within it, Fail when it lies wholly above, else Unknown.
fn band_verdict(bound: f64, band: Option<&str>) -> RuleVerdict {
    let Some((_, lower, upper)) = band.and_then(|band| {
        crate::rule_engine::INCOME_MEDIAN_BANDS
            .iter()
            .find(|(code, _, _)| *code == band)
    }) else {
        return RuleVerdict::Unknown;
    };
    if *upper <= bound {
        RuleVerdict::Pass
    } else if *lower >= bound {
        RuleVerdict::Fail
    } else {
        RuleVerdict::Unknown
    }
}

/// The profile field that settles an unanswered income check.
pub const ANSWER_FIELD: &str = "income_median_pct";

/// Status words: a notice that names one restricts to a status or group
/// (수급자, 차상위, 한부모, 장애인, 피해자, 취약계층, 북한이탈주민, 다문화,
/// 보훈·국가유공자, …) that an income band does not prove.
pub const STATUS_WORDS: [&str; 19] = [
    "수급",
    "차상위",
    "기초생활",
    "생계급여",
    "의료급여",
    "주거급여",
    "교육급여",
    "한부모",
    "기초연금",
    "긴급복지",
    "장애",
    "피해자",
    "피구조자",
    "저소득층",
    "취약계층",
    "북한이탈",
    "다문화",
    "보훈",
    "국가유공",
];

/// The named check of a status the band cannot prove.
pub const STATUS_CHECK_LABEL: &str = "수급자·차상위 등 자격 확인 필요";

/// SQL: whether the row has coded 보조금24 bands (`income_median_bands` a
/// non-empty array), the rows whose text [`check`] reads: a status word and
/// a stated bound decide nothing on any other row. The decision query reads
/// the text of these rows only, so a request does not scan every notice's
/// text (follow-up 9: two thirds of the query's time on the full catalogue).
const CODED_BANDS_SQL: &str = "(jsonb_typeof(normalized_payload->'income_median_bands') = 'array' \
     AND normalized_payload->'income_median_bands' <> '[]'::jsonb)";

/// SQL: whether the row's summary, 지원대상, 선정기준 or 지원내용 names a
/// [`STATUS_WORDS`] word (the decision query reduces the payload to this
/// flag, `income_status_text`); `false` on a row without coded bands.
pub fn status_text_sql() -> String {
    format!(
        "(CASE WHEN {CODED_BANDS_SQL} THEN COALESCE(concat_ws(' ', summary, raw_payload->>'지원대상', \
         raw_payload->>'선정기준', raw_payload->>'지원내용') ~ '({})', false) ELSE false END)",
        STATUS_WORDS.join("|")
    )
}

/// Whether the programme's text names a status word: the decision query's
/// flag when present, else the loaded text itself. Reading text here can
/// only withhold a Pass, never grant one.
pub fn status_named(program: &Program) -> bool {
    if let Some(flag) = program
        .normalized_payload
        .as_ref()
        .and_then(|p| p.get("income_status_text"))
        .and_then(Value::as_bool)
    {
        return flag;
    }
    let raw = program.raw_payload.as_ref();
    let field = |key: &str| raw.and_then(|r| r.get(key)).and_then(Value::as_str);
    [
        program.summary.as_deref(),
        field("지원대상"),
        field("선정기준"),
        field("지원내용"),
    ]
    .into_iter()
    .flatten()
    .any(|text| STATUS_WORDS.iter().any(|word| text.contains(word)))
}

/// How far after "중위소득" (and a parenthetical right after it) a stated
/// percentage may start ("중위소득의 ", "중위소득(가구원수별) ").
const BOUND_WINDOW_CHARS: usize = 16;
/// The text after one "중위소득" that is read: up to the next '중' (so one
/// window never swallows the next "중위소득") and at most this many chars,
/// enough for a parenthetical ("(4인가구 기준 6,097,773원)의"), the number,
/// its sign and the words after it.
const BOUND_CLIP_CHARS: usize = 48;
/// Words after a stated bound that make it someone left out or preferred,
/// not who is eligible ("50% 이하 제외", "100% 이하는 우선 선발").
const NOT_ELIGIBLE_AFTER: [&str; 4] = ["제외", "우선", "우대", "가점"];
/// How many characters after the bound word [`NOT_ELIGIBLE_AFTER`] is
/// looked for.
const AFTER_WORD_CHARS: usize = 8;

/// What a stated percentage bounds, from the word right after its sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundKind {
    /// 이하, 이내, 미만: an upper bound.
    Ceiling,
    /// 초과, 이상, or the low end of a range ("80~100%"): a lower bound.
    Floor,
    /// Anything else ("50%의 금액", "50% 이하 제외").
    Bare,
}

/// The percentages stated right after each "중위소득" (or "중위 소득") in
/// `text` ("기준 중위소득 50% 이하" → (50, Ceiling)). The one reader of a
/// stated bound; the decision query only clips the same windows
/// ([`bound_text_sql`]). Read: `%`, `％` or 퍼센트, 100분의 N, full-width
/// digits, a parenthetical before the number, and a range ("A~B%": a floor
/// and a ceiling).
pub fn stated_bounds(text: &str) -> Vec<(u32, BoundKind)> {
    windows(text).flat_map(window_bounds).collect()
}

/// The text after each "중위" + whitespace + "소득" in `text`, as the
/// decision query's `중위\s*소득` matches it.
fn windows(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || loop {
        let at = rest.find("중위")?;
        let after = &rest[at + "중위".len()..];
        let spaced = after.trim_start_matches(char::is_whitespace);
        match spaced.strip_prefix("소득") {
            Some(window) => {
                rest = window;
                return Some(window);
            }
            None => rest = after,
        }
    })
}

/// `c` with full-width digits and signs folded to ASCII.
fn fold(c: char) -> char {
    match c {
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
        _ => c,
    }
}

/// The bounds stated at the start of the text after one "중위소득".
fn window_bounds(after: &str) -> Vec<(u32, BoundKind)> {
    let clip: Vec<char> = after
        .chars()
        .take_while(|c| *c != '중')
        .take(BOUND_CLIP_CHARS)
        .map(fold)
        .collect();
    // A parenthetical right after the word ("(가구원수별)", "(4인가구 기준
    // 6,097,773원)") is skipped, so its numbers are not the bound.
    let from = match clip.iter().position(|c| !c.is_whitespace()) {
        Some(open) if clip[open] == '(' => clip[open..]
            .iter()
            .position(|c| *c == ')')
            .map_or(clip.len(), |close| open + close + 1),
        _ => 0,
    };
    let Some(start) =
        (from..clip.len().min(from + BOUND_WINDOW_CHARS)).find(|i| clip[*i].is_ascii_digit())
    else {
        return Vec::new();
    };
    let Some((first, rest)) = number(&clip[start..]) else {
        return Vec::new();
    };
    let rest = skip_spaces(rest);
    // 100분의 N: N percent (only a denominator of 100 is a percentage).
    if let Some(after) = rest.strip_prefix(&['분', '의'][..]) {
        return match number(skip_spaces(after)) {
            Some((n, after)) if first == 100 => vec![(n, kind(skip_spaces(after)))],
            _ => Vec::new(),
        };
    }
    // A range "A~B%" or "A%~B%": A is a floor, B is read as any bound.
    let (rest, low_sign) = match percent(rest) {
        Some(after) => (skip_spaces(after), true),
        None => (rest, false),
    };
    if let Some(after) = rest
        .strip_prefix(&['~'][..])
        .or_else(|| rest.strip_prefix(&['∼'][..]))
    {
        let after = skip_spaces(after);
        return match number(after) {
            Some((high, after)) => match percent(skip_spaces(after)) {
                Some(after) => vec![(first, BoundKind::Floor), (high, kind(skip_spaces(after)))],
                None => Vec::new(),
            },
            None => Vec::new(),
        };
    }
    if !low_sign {
        return Vec::new();
    }
    vec![(first, kind(rest))]
}

/// A run of ASCII digits at the start of `chars` and what follows it.
fn number(chars: &[char]) -> Option<(u32, &[char])> {
    let len = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let digits: String = chars[..len].iter().collect();
    digits.parse().ok().map(|n| (n, &chars[len..]))
}

fn skip_spaces(chars: &[char]) -> &[char] {
    let n = chars
        .iter()
        .take_while(|c| **c == ' ' || **c == '\u{a0}')
        .count();
    &chars[n..]
}

/// The text after a percent sign at the start of `chars` (`%`, 퍼센트).
fn percent(chars: &[char]) -> Option<&[char]> {
    chars
        .strip_prefix(&['%'][..])
        .or_else(|| chars.strip_prefix(&['퍼', '센', '트'][..]))
}

/// What the words right after a bound make of it.
fn kind(rest: &[char]) -> BoundKind {
    let starts = |words: &[&str]| {
        words.iter().any(|w| {
            let w: Vec<char> = w.chars().collect();
            rest.starts_with(&w)
        })
    };
    let kind = if starts(&["이하", "이내", "미만"]) {
        BoundKind::Ceiling
    } else if starts(&["초과", "이상"]) {
        BoundKind::Floor
    } else {
        BoundKind::Bare
    };
    let after: String = rest.iter().skip(2).take(AFTER_WORD_CHARS).collect();
    if kind == BoundKind::Ceiling && NOT_ELIGIBLE_AFTER.iter().any(|w| after.contains(w)) {
        BoundKind::Bare
    } else {
        kind
    }
}

/// The text fields a stated bound is read from, one by one, in the order
/// [`bound_text_sql`] clips them.
fn bound_fields(program: &Program) -> [Option<&str>; 4] {
    let raw = program.raw_payload.as_ref();
    let field = |key: &str| raw.and_then(|r| r.get(key)).and_then(Value::as_str);
    [
        program.summary.as_deref(),
        field("지원대상"),
        field("선정기준"),
        field("지원내용"),
    ]
}

/// SQL: the clipped text after each "중위소득" (`중위\s*소득`, up to the next
/// '중', at most [`BOUND_CLIP_CHARS`] chars) in each of the row's summary,
/// 지원대상, 선정기준 and 지원내용 (field by field, as [`stated_bounds`]
/// reads loaded text), as a JSON array, empty on a row without coded bands
/// ([`CODED_BANDS_SQL`]); the decision query stores it as
/// `income_bound_text`. Binds nothing.
pub fn bound_text_sql() -> String {
    format!(
        "(CASE WHEN {CODED_BANDS_SQL} THEN to_jsonb(ARRAY(SELECT m[1] FROM unnest(ARRAY[summary, \
         raw_payload->>'지원대상', raw_payload->>'선정기준', raw_payload->>'지원내용']) AS f(t), \
         regexp_matches(f.t, '중위\\s*소득([^중]{{0,{BOUND_CLIP_CHARS}}})', 'g') AS m)) \
         ELSE '[]'::jsonb END)"
    )
}

/// The one income ceiling the notice states: every stated ceiling is the
/// same number and no floor is stated. Reads the decision query's windows
/// when present, else the loaded text itself.
fn stated_ceiling(program: &Program) -> Option<u32> {
    let stated: Vec<(u32, BoundKind)> = match program
        .normalized_payload
        .as_ref()
        .and_then(|p| p.get("income_bound_text"))
        .and_then(Value::as_array)
    {
        Some(windows) => windows
            .iter()
            .filter_map(Value::as_str)
            .flat_map(window_bounds)
            .collect(),
        None => bound_fields(program)
            .into_iter()
            .flatten()
            .flat_map(stated_bounds)
            .collect(),
    };
    if stated.iter().any(|(_, kind)| *kind == BoundKind::Floor) {
        return None;
    }
    let mut ceilings = stated
        .iter()
        .filter(|(_, kind)| *kind == BoundKind::Ceiling)
        .map(|(n, _)| *n);
    let first = ceilings.next()?;
    ceilings.all(|n| n == first).then_some(first)
}

/// The bound the income check decides on: the notice's one stated ceiling
/// ([`stated_ceiling`]) when the coded 보조금24 run contains it. The five
/// coded bands are coarse ("중위소득 150% 이하" is coded up to 200 %), so
/// the stated number decides; a ceiling outside the top coded band (code
/// 0-50, text 52 %) is a disagreement and decides nothing. `None` when the
/// row has no coded run or the text states no such ceiling.
pub fn decision_bound(program: &Program) -> Option<f64> {
    let Some(IncomeCondition::MedianBands(bands)) = condition(program) else {
        return None;
    };
    let code = median_bound(&bands)?;
    let ceiling = f64::from(stated_ceiling(program)?);
    let floor = MEDIAN_RUN
        .iter()
        .take_while(|(_, bound)| *bound < code)
        .last()
        .map_or(0.0, |(_, bound)| *bound);
    (ceiling > floor && ceiling <= code).then_some(ceiling)
}

/// The income check of one programme for one band answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub verdict: RuleVerdict,
    /// The band is within the bound, but the notice names a status the band
    /// does not prove: Unknown with [`STATUS_CHECK_LABEL`].
    pub status_unconfirmed: bool,
}

/// The check, decided only on [`decision_bound`]: the coded flags are loose
/// (module docs), so a bound the text does not state is Unknown both ways.
/// Pass and Fail cost differently (review B4):
/// - a Pass on a programme whose text names a status word is Unknown;
/// - a Fail hides, so it needs the text ceiling to be the coded bound itself
///   and no status word. A smaller ceiling inside the coded band may be one
///   route of several ("… 이하 또는 다문화가족"), a priority or a rate tier,
///   and a status word may be another route: both are Unknown.
///
/// [`STATUS_WORDS`] only pick the label. `None` when the programme has no
/// coded income condition.
pub fn check(program: &Program, band: Option<&str>) -> Option<Check> {
    let condition = condition(program)?;
    let status = status_named(program);
    let coded_bound = match &condition {
        IncomeCondition::MedianBands(bands) => median_bound(bands),
        _ => None,
    };
    let Some(bound) = decision_bound(program) else {
        // Undecided: the status label only where the code alone would
        // have decided (a band on a contiguous run).
        return Some(Check {
            verdict: RuleVerdict::Unknown,
            status_unconfirmed: status && verdict(&condition, band) != RuleVerdict::Unknown,
        });
    };
    let verdict = band_verdict(bound, band);
    let hides_on_the_code = !status && coded_bound == Some(bound);
    Some(if verdict == RuleVerdict::Pass && status {
        Check {
            verdict: RuleVerdict::Unknown,
            status_unconfirmed: true,
        }
    } else if verdict == RuleVerdict::Fail && !hides_on_the_code {
        Check {
            verdict: RuleVerdict::Unknown,
            status_unconfirmed: status,
        }
    } else {
        Check {
            verdict,
            status_unconfirmed: false,
        }
    })
}

/// The keys of `normalized_payload` [`condition`] reads, as a SQL
/// `jsonb_build_object` argument list for queries that reduce the payload.
pub const PAYLOAD_KEYS_SQL: &str = "'income_median_bands', normalized_payload->'income_median_bands', \
     'youth_conditions', jsonb_build_object(\
       'income_annual_min_manwon', normalized_payload->'youth_conditions'->'income_annual_min_manwon', \
       'income_annual_max_manwon', normalized_payload->'youth_conditions'->'income_annual_max_manwon', \
       'income_condition_text', normalized_payload->'youth_conditions'->'income_condition_text')";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with(payload: Value) -> Program {
        let mut p = crate::structural::test_program("policy");
        p.normalized_payload = Some(payload);
        p
    }

    #[test]
    fn coded_income_fields_are_conditions() {
        assert_eq!(
            condition(&with(json!({"income_median_bands": ["0-50"]}))),
            Some(IncomeCondition::MedianBands(vec!["0-50".into()]))
        );
        assert_eq!(
            condition(&with(
                json!({"youth_conditions": {"income_annual_max_manwon": 5000}})
            )),
            Some(IncomeCondition::AnnualIncome {
                min: None,
                max: Some(5000)
            })
        );
        assert_eq!(
            condition(&with(
                json!({"youth_conditions": {"income_condition_text": "중위소득 150% 이하"}})
            )),
            Some(IncomeCondition::Described)
        );
    }

    #[test]
    fn nothing_coded_is_no_condition() {
        for payload in [
            json!({}),
            json!({"income_median_bands": null}),
            json!({"income_median_bands": []}),
            json!({"youth_conditions": {"income_annual_max_manwon": null,
                                        "income_condition_text": null}}),
            json!({"youth_conditions": {"income_condition_text": "  "}}),
            // Text elsewhere is never read.
            json!({"conditions": "중위소득 100% 이하"}),
        ] {
            assert_eq!(condition(&with(payload.clone())), None, "{payload}");
        }
        assert_eq!(condition(&crate::structural::test_program("policy")), None);
    }

    #[test]
    fn a_status_defined_row_never_passes_on_the_band() {
        let mut p = with(json!({"income_median_bands": ["0-50"]}));
        p.summary = Some("기준 중위소득 50% 이하 가구 지원".into());
        assert_eq!(
            check(&p, Some("le50")),
            Some(Check {
                verdict: RuleVerdict::Pass,
                status_unconfirmed: false
            })
        );
        p.summary = None;
        p.raw_payload = Some(json!({"선정기준": "국민기초생활보장 수급자 및 차상위계층"}));
        assert_eq!(
            check(&p, Some("le50")),
            Some(Check {
                verdict: RuleVerdict::Unknown,
                status_unconfirmed: true
            })
        );
        // The text states no bound: a band above it is a named check, not a
        // hide (audit M1). With the bound stated and a status word, the
        // status may be another route: still a check (review B4).
        assert_eq!(
            check(&p, Some("le60")),
            Some(Check {
                verdict: RuleVerdict::Unknown,
                status_unconfirmed: true
            })
        );
        let mut stated = p.clone();
        stated.raw_payload =
            Some(json!({"선정기준": "수급자 및 차상위계층(기준 중위소득 50% 이하)"}));
        assert_eq!(
            check(&stated, Some("le60")),
            Some(Check {
                verdict: RuleVerdict::Unknown,
                status_unconfirmed: true
            })
        );
        // No answer stays plain Unknown.
        assert_eq!(
            check(&p, None),
            Some(Check {
                verdict: RuleVerdict::Unknown,
                status_unconfirmed: false
            })
        );
        // The summary and the decision query's flag count too.
        let mut q = with(json!({"income_median_bands": ["0-50"]}));
        q.summary = Some("한부모가족 생활 지원".into());
        assert!(status_named(&q));
        let flagged = with(json!({"income_median_bands": ["0-50"], "income_status_text": true}));
        assert!(status_named(&flagged));
        let clear = with(json!({"income_median_bands": ["0-50"], "income_status_text": false}));
        assert!(!status_named(&clear));
        assert!(status_text_sql().contains("차상위"));
    }

    #[test]
    fn group_words_also_withhold_a_pass() {
        for text in [
            "등록 장애인 가구",
            "범죄 피해자 지원",
            "피구조자 생활 지원",
            "저소득층 가구",
            "취약계층 대상",
            "한부모가족",
            "북한이탈주민",
            "다문화가족",
            "보훈대상자",
            "국가유공자 유족",
        ] {
            let mut p = with(json!({"income_median_bands": ["0-50"]}));
            p.raw_payload = Some(json!({"지원대상": text}));
            assert_eq!(
                check(&p, Some("le50")),
                Some(Check {
                    verdict: RuleVerdict::Unknown,
                    status_unconfirmed: true
                }),
                "{text}"
            );
        }
        // A plain income bound still passes.
        let mut plain = with(json!({"income_median_bands": ["0-50"]}));
        plain.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 청년"}));
        assert_eq!(
            check(&plain, Some("le50")).unwrap().verdict,
            RuleVerdict::Pass
        );
    }

    /// Audit M1: the 보조금24 flags are loose (243 of 1,203 "0-50 only" rows
    /// name no income), so a coded bound decides only when the notice text
    /// states it.
    #[test]
    fn a_bare_flag_never_hides() {
        let mut p = with(json!({"income_median_bands": ["0-50"]}));
        p.raw_payload = Some(json!({"지원대상": "관내 거주 청년 누구나"}));
        assert_eq!(
            check(&p, Some("le60")),
            Some(Check {
                verdict: RuleVerdict::Unknown,
                status_unconfirmed: false
            })
        );
        // Stated: the band decides both ways.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 청년"}));
        assert_eq!(check(&p, Some("le60")).unwrap().verdict, RuleVerdict::Fail);
        assert_eq!(check(&p, Some("le50")).unwrap().verdict, RuleVerdict::Pass);
        // Another percentage is not this bound.
        p.raw_payload = Some(json!({"선정기준": "기준 중위소득 150% 이하"}));
        assert_eq!(
            check(&p, Some("le60")).unwrap().verdict,
            RuleVerdict::Unknown
        );
    }

    /// Review R-M2: a stated floor, or a coded bound that is one tier of
    /// several, never decides; only the largest stated ceiling does.
    #[test]
    fn a_floor_or_one_tier_of_several_never_decides() {
        let mut p = with(json!({"income_median_bands": ["0-50"]}));
        p.raw_payload = Some(json!({"지원대상": "중위소득 50% 초과 100% 이하 청년"}));
        for band in ["le50", "le60"] {
            assert_eq!(
                check(&p, Some(band)).unwrap().verdict,
                RuleVerdict::Unknown,
                "{band}"
            );
        }
        p.raw_payload = Some(json!({
            "지원대상": "중위소득 50% 이하 가구는 전액, 중위소득 120% 이하는 일부 지원"
        }));
        assert_eq!(
            check(&p, Some("le100")).unwrap().verdict,
            RuleVerdict::Unknown
        );
        // One stated ceiling equal to the code still decides both ways.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이내 가구"}));
        assert_eq!(check(&p, Some("le60")).unwrap().verdict, RuleVerdict::Fail);
        assert_eq!(check(&p, Some("le50")).unwrap().verdict, RuleVerdict::Pass);
    }

    #[test]
    fn a_status_row_without_a_listed_word_does_not_pass() {
        let mut p = with(json!({"income_median_bands": ["0-50"]}));
        p.raw_payload = Some(json!({"지원대상": "자립준비청년(보호종료아동)"}));
        assert_eq!(
            check(&p, Some("le50")).unwrap().verdict,
            RuleVerdict::Unknown
        );
    }

    /// The decision query reduces the text to the windows after each
    /// "중위소득"; the same scanner reads them.
    #[test]
    fn the_stated_bound_reads_the_text_or_the_query_windows() {
        assert_eq!(
            stated_bounds("중위소득 50% 또는 중위소득 75% 이하"),
            [(50, BoundKind::Bare), (75, BoundKind::Ceiling)]
        );
        for (text, bound) in [
            ("기준 중위소득 50% 이하", Some(50)),
            ("기준중위소득의 75 % 이하 가구", Some(75)),
            ("중위소득 100％ 이하", Some(100)),
            ("중위소득(가구원수별) 120% 이하", Some(120)),
            ("중위소득 기준 충족자 50명", None),
            ("중위소득", None),
        ] {
            assert_eq!(
                stated_bounds(text).first().map(|(n, _)| *n),
                bound,
                "{text}"
            );
        }
        let mut p = with(json!({
            "income_median_bands": ["0-50"],
            "income_bound_text": [" 50% 이하 가구"]
        }));
        assert_eq!(check(&p, Some("le60")).unwrap().verdict, RuleVerdict::Fail);
        p.normalized_payload = Some(json!({
            "income_median_bands": ["0-50"],
            "income_bound_text": []
        }));
        // The query's windows win over (absent) loaded text.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하"}));
        assert_eq!(
            check(&p, Some("le60")).unwrap().verdict,
            RuleVerdict::Unknown
        );
        assert!(bound_text_sql().contains("중위\\s*소득"));
    }

    /// Review R-L3 / review2 L8: every shape that states one bound
    /// unambiguously is read; a range states a floor, and a bound followed
    /// by 제외/우선 names who is left out or preferred, not who is eligible.
    #[test]
    fn stated_bounds_read_every_unambiguous_shape() {
        use BoundKind::*;
        for (text, bounds) in [
            ("기준중위소득 52퍼센트 이하인 가구", vec![(52, Ceiling)]),
            (
                "저소득주민 : 중위소득의 100분의 60이하인 자",
                vec![(60, Ceiling)],
            ),
            ("기준 중위 소득 150% 이하", vec![(150, Ceiling)]),
            (
                "기준중위소득(4인가구 기준 6,097,773원)의 50% 이하",
                vec![(50, Ceiling)],
            ),
            ("중위소득 ５０％ 이하", vec![(50, Ceiling)]),
            (
                "기준중위소득 120%~160%이하인 아동",
                vec![(120, Floor), (160, Ceiling)],
            ),
            (
                "통상 중위소득 80~100% 이하",
                vec![(80, Floor), (100, Ceiling)],
            ),
            ("중위소득 100% 이하는 우선 선발", vec![(100, Bare)]),
            ("중위소득 50% 이하 제외", vec![(50, Bare)]),
            ("기준 중위소득 50% 이하", vec![(50, Ceiling)]),
            ("중위소득 기준 충족자 50명", vec![]),
            ("중위소득의 1000분의 600 이하", vec![]),
        ] {
            assert_eq!(stated_bounds(text), bounds, "{text}");
        }
    }

    /// The 보조금24 bands are five coarse runs, so a notice's own ceiling
    /// (150 %) is coded as the run that contains it (to 200 %). The stated
    /// ceiling decides when the code contains it; a ceiling outside the
    /// coded run, or two ceilings, stays unknown.
    #[test]
    fn a_stated_ceiling_inside_the_coded_run_decides_on_itself() {
        let mut p = with(json!({"income_median_bands": ["0-50", "51-75", "76-100", "101-200"]}));
        p.raw_payload = Some(json!({"지원대상": "소득인정액이 기준 중위소득 150% 이하 가정"}));
        // It passes below itself; above it, it is not the coded bound, so it
        // never hides (review B4).
        for (band, verdict) in [
            ("le120", RuleVerdict::Pass),
            ("le150", RuleVerdict::Pass),
            ("le180", RuleVerdict::Unknown),
            ("gt180", RuleVerdict::Unknown),
        ] {
            assert_eq!(check(&p, Some(band)).unwrap().verdict, verdict, "{band}");
        }
        assert_eq!(decision_bound(&p), Some(150.0));
        // 52 % is not inside the coded 0-50 run: the code and the text
        // disagree.
        let mut q = with(json!({"income_median_bands": ["0-50"]}));
        q.raw_payload = Some(json!({"지원대상": "기준중위소득 52% 이하인 가구의 아동"}));
        for band in ["le50", "le60"] {
            assert_eq!(check(&q, Some(band)).unwrap().verdict, RuleVerdict::Unknown);
        }
        // Two ceilings are tiers.
        let mut r = with(json!({"income_median_bands": ["0-50", "51-75", "76-100"]}));
        r.raw_payload =
            Some(json!({"지원대상": "중위소득 60% 이하(방과후자유수강권은 중위소득 90% 이하)"}));
        assert_eq!(
            check(&r, Some("le50")).unwrap().verdict,
            RuleVerdict::Unknown
        );
        assert_eq!(decision_bound(&r), None);
    }

    /// Review B4: a ceiling inside the coded band may be one route of
    /// several, a priority or a rate tier. Such a ceiling may pass a band
    /// below it but never hides one above it; a hide needs the text ceiling
    /// to be the coded bound itself and no status word.
    #[test]
    fn only_the_coded_bound_itself_hides() {
        let all = json!({"income_median_bands": ["0-50", "51-75", "76-100", "101-200"]});
        // Coded ≤100 and stated 100, but one route of several (status words).
        let mut p = with(json!({"income_median_bands": ["0-50", "51-75", "76-100"]}));
        p.raw_payload = Some(
            json!({"지원대상": "기준 중위소득 100% 이하 가구 또는 다문화가족, 장애인 가구의 청년"}),
        );
        assert_ne!(check(&p, Some("le150")).unwrap().verdict, RuleVerdict::Fail);
        for (field, text, band) in [
            (
                "지원대상",
                "기준 중위소득 150% 이하(맞벌이 가구는 180% 이하) 무주택 청년",
                "le180",
            ),
            ("지원내용", "기준 중위소득 120% 이하: 지원율 90%", "le150"),
            (
                "선정기준",
                "기준 중위소득 150% 이하인 자를 1순위로 우선 선발",
                "le180",
            ),
            (
                "지원대상",
                "도시근로자 월평균소득 100% 이하 또는 기준 중위소득 150% 이하",
                "le180",
            ),
            (
                "지원대상",
                "본인 소득 기준 중위소득 120% 이하, 부모 포함 가구 180% 이하",
                "le150",
            ),
        ] {
            let mut p = with(all.clone());
            p.raw_payload = Some(json!({ field: text }));
            assert_ne!(
                check(&p, Some(band)).unwrap().verdict,
                RuleVerdict::Fail,
                "{text} / {band}"
            );
        }
        // The coded bound stated as itself still decides both ways.
        let mut p = with(json!({"income_median_bands": ["0-50"]}));
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 청년"}));
        assert_eq!(check(&p, Some("le60")).unwrap().verdict, RuleVerdict::Fail);
        assert_eq!(check(&p, Some("le50")).unwrap().verdict, RuleVerdict::Pass);
    }

    #[test]
    fn the_bound_is_a_contiguous_run_from_zero() {
        let b = |v: &[&str]| median_bound(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(b(&["0-50"]), Some(50.0));
        assert_eq!(b(&["51-75", "0-50"]), Some(75.0));
        assert_eq!(b(&["0-50", "51-75", "76-100"]), Some(100.0));
        assert_eq!(b(&["0-50", "51-75", "76-100", "101-200"]), Some(200.0));
        for gap in [
            &["0-50", "76-100"][..],
            &["101-200", "200+"],
            &["76-100"],
            &["0-50", "200+"],
            &[],
        ] {
            assert_eq!(b(gap), None, "{gap:?}");
        }
    }

    #[test]
    fn a_band_passes_within_fails_above_and_is_unknown_across_the_bound() {
        let bands =
            |v: &[&str]| IncomeCondition::MedianBands(v.iter().map(|s| s.to_string()).collect());
        let le50 = bands(&["0-50"]);
        let le100 = bands(&["0-50", "51-75", "76-100"]);
        assert_eq!(verdict(&le50, Some("le50")), RuleVerdict::Pass);
        assert_eq!(verdict(&le50, Some("le60")), RuleVerdict::Fail);
        assert_eq!(verdict(&le50, Some("gt180")), RuleVerdict::Fail);
        assert_eq!(verdict(&le100, Some("le60")), RuleVerdict::Pass);
        assert_eq!(verdict(&le100, Some("le100")), RuleVerdict::Pass);
        assert_eq!(verdict(&le100, Some("le120")), RuleVerdict::Fail);
        // (60, 100] against a 75 % bound straddles it.
        assert_eq!(
            verdict(&bands(&["0-50", "51-75"]), Some("le100")),
            RuleVerdict::Unknown
        );
        // No answer or 모름: never Pass.
        assert_eq!(verdict(&le50, None), RuleVerdict::Unknown);
        assert_eq!(verdict(&le50, Some("unknown")), RuleVerdict::Unknown);
        assert_eq!(verdict(&le50, Some("typo")), RuleVerdict::Unknown);
        // Gaps and the 온통청년 conditions never decide.
        assert_eq!(
            verdict(&bands(&["0-50", "76-100"]), Some("le50")),
            RuleVerdict::Unknown
        );
        let annual = IncomeCondition::AnnualIncome {
            min: None,
            max: Some(5000),
        };
        assert_eq!(verdict(&annual, Some("le50")), RuleVerdict::Unknown);
        assert_eq!(
            verdict(&IncomeCondition::Described, Some("le50")),
            RuleVerdict::Unknown
        );
    }
}
