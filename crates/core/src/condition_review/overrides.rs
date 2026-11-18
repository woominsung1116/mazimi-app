//! Reviewed overrides that widen a source-code rule (`code_overrides`).
//!
//! 온통청년 codes become rules that hide a programme: the served columns
//! (`min_age`/`max_age`, `regions`, `region_districts`, the
//! `enrollment_statuses` of `normalized_payload`) and the compiled
//! `eligibility_rules` source conditions (`origin: source_codes`). Where the
//! notice text admits more people than the code (군 경력자 up to 39, 인접
//! 시도, 불완전 취업자, "재학생 및 지역 청년"), a current review may override
//! the code rule:
//!
//! - `widen`: the code rule is replaced by `replacement`, which must be
//!   provably at least as wide ([`contains`]);
//! - `drop`: the code rule no longer hides.
//!
//! Either way the override carries a `check_kind` that becomes a remaining
//! check, and a row with an applied override is never "조건 확인됨": an
//! override marks a place where the codes and the text disagree, and whether
//! a replacement admits exactly what the text admits is not machine-checked.
//!
//! An override is applied only while its review is current (text hash) and
//! only while the target still exists and the replacement still contains it
//! ([`applicable`]); otherwise the code rule hides as before.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::models::{ExclusionReason, Program};
use crate::rule_engine::{self, EligibilityProfile, EvalDetail, RuleEvalResult, RuleVerdict};
use crate::structural::{self, CheckKind, StructuralCheck, StructuralProfile};

/// Code rule ids an override may target, as the curation input names them
/// (`segment.py` `structural_rules` / `youth_code_rules`). `C-marital` and
/// `C-income-*` are not compiled into any hiding rule, so there is nothing
/// to override.
pub const TARGETS: [&str; 8] = [
    "S-age",
    "S-region",
    "S-district",
    "S-enrollment",
    "C-job",
    "C-school",
    "C-sbiz",
    "C-major",
];

/// Profile values the structural enrollment check compares against.
const ENROLLMENT_STATUSES: [&str; 4] = ["enrolled", "on_leave", "graduated", "research_student"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Widen,
    Drop,
}

/// One validated override as stored in `program_condition_reviews.code_overrides`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeOverride {
    pub target: String,
    pub action: Action,
    /// `widen` only: a leaf or any-group in the review vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Value>,
    /// Contract check kind served as a remaining check.
    pub check_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seg_id: Option<String>,
    /// Verbatim quote of the current text.
    pub evidence: String,
}

impl CodeOverride {
    /// The structural check an `S-` target replaces; `None` for `C-`.
    pub fn structural_kind(&self) -> Option<CheckKind> {
        Some(match self.target.as_str() {
            "S-age" => CheckKind::Age,
            "S-region" => CheckKind::Region,
            "S-district" => CheckKind::District,
            "S-enrollment" => CheckKind::Enrollment,
            _ => return None,
        })
    }

    /// Why the list hides a row whose replacement the profile contradicts:
    /// the target's own reason, so the filter summary counts it as before.
    pub fn exclusion(&self) -> ExclusionReason {
        match self.structural_kind() {
            Some(CheckKind::Age) => ExclusionReason::Age,
            Some(CheckKind::Region | CheckKind::District) => ExclusionReason::Region,
            Some(CheckKind::Enrollment) => ExclusionReason::Enrollment,
            _ => ExclusionReason::HardRule,
        }
    }
}

/// Remaining-check kind of a row listed only because an override admitted
/// the profile ([`admitting`]). The list ranks such rows last.
pub const EXCEPTION_KIND: &str = "code_exception";

/// Fixed copy of the [`EXCEPTION_KIND`] check for a target: the user fails
/// the code rule and is listed on the text's exception, which nothing in
/// the profile confirms.
pub fn exception_label(target: &str) -> &'static str {
    match target {
        "S-age" => "연령 조건 예외에 해당하는지 확인이 필요해요",
        "S-region" | "S-district" => "지역 조건 예외에 해당하는지 확인이 필요해요",
        "S-enrollment" => "재학 상태 조건 예외에 해당하는지 확인이 필요해요",
        "C-job" => "취업 상태 조건 예외에 해당하는지 확인이 필요해요",
        "C-school" => "학력 조건 예외에 해당하는지 확인이 필요해요",
        "C-major" => "전공 조건 예외에 해당하는지 확인이 필요해요",
        _ => "대상 조건 예외에 해당하는지 확인이 필요해요",
    }
}

/// Stored `code_overrides` (NULL = none). `Err` for a malformed value.
pub fn parse_stored(value: Option<&Value>) -> Result<Vec<CodeOverride>, serde_json::Error> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone()),
    }
}

/// Fields of the compiled source conditions a `C-` target names.
fn source_fields(target: &str) -> Option<&'static [&'static str]> {
    Some(match target {
        "C-job" => &["employment_status"],
        "C-school" => &["education_stage"],
        "C-sbiz" => &["has_disability", "is_low_income_household"],
        "C-major" => &["major_category"],
        _ => return None,
    })
}

/// The programme facts the code rules are read from.
#[derive(Debug, Clone, Default)]
pub struct CodeColumns {
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub region_districts: Option<Vec<String>>,
    /// `normalized_payload.enrollment_statuses`.
    pub enrollment_statuses: Option<Vec<String>>,
    /// The active `eligibility_rules.hard_filter_json`.
    pub hard_filter: Option<Value>,
}

impl CodeColumns {
    pub fn of(program: &Program, hard_filter: Option<&Value>) -> Self {
        Self {
            min_age: program.min_age,
            max_age: program.max_age,
            regions: program.regions.clone(),
            region_districts: program.region_districts.clone(),
            enrollment_statuses: structural::enrollment_statuses(program).map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
            hard_filter: hard_filter.cloned(),
        }
    }
}

/// Every field a rule node reads.
fn leaf_fields(node: &Value) -> Vec<&str> {
    match (node.get("any"), node.get("all")) {
        (Some(Value::Array(children)), _) | (_, Some(Value::Array(children))) => {
            children.iter().flat_map(leaf_fields).collect()
        }
        _ => node
            .get("field")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
    }
}

/// The top-level source conditions of `tree` a `C-` target names: every
/// field they read is one of the target's.
fn matching_conditions<'a>(tree: Option<&'a Value>, target: &str) -> Vec<&'a Value> {
    let Some(fields) = source_fields(target) else {
        return Vec::new();
    };
    rule_engine::top_level_conditions(tree)
        .into_iter()
        .filter(|c| {
            let read = leaf_fields(c);
            rule_engine::is_source_condition(c)
                && !read.is_empty()
                && read.iter().all(|f| fields.contains(f))
        })
        .collect()
}

/// The current code rule `target` names, as a rule tree, or `None` when the
/// programme has no such rule. `S-district` is the exact district rule of
/// [`structural::district_check`]: users of a listed sido without listed
/// districts pass, others need a covered `city_code` (a `city_code` leaf
/// here always means that district coverage, see [`evaluate_replacement`]).
pub fn target_rule(target: &str, columns: &CodeColumns) -> Option<Value> {
    match target {
        "S-age" => match (columns.min_age, columns.max_age) {
            (Some(lo), Some(hi)) => {
                Some(json!({"field": "age", "op": "between", "value": [lo, hi]}))
            }
            (Some(lo), None) => Some(json!({"field": "age", "op": "gte", "value": lo})),
            (None, Some(hi)) => Some(json!({"field": "age", "op": "lte", "value": hi})),
            (None, None) => None,
        },
        "S-region" => {
            let regions = columns.regions.as_ref().filter(|r| !r.is_empty())?;
            // An unresolved scope ("unknown") hides everyone. Any explicit
            // region list contains it ([`contains`]); the importer accepts
            // only that (`widen` to a `region_code` `in`/`eq` leaf), never a
            // `drop` or an all-but set, and [`applicable`] ignores a `drop`
            // of it.
            Some(json!({"field": "region_code", "op": "in", "value": regions}))
        }
        "S-district" => {
            // The current codes the structural check reads. An unresolved
            // entry has no rule leaf; a reviewed replacement supersedes it.
            let codes = structural::programme_districts(columns.region_districts.as_ref()?).codes;
            if codes.is_empty() {
                return None;
            }
            let sido_wide: Vec<&String> = columns
                .regions
                .iter()
                .flatten()
                .filter(|r| {
                    !codes
                        .iter()
                        .any(|c| structural::district_region(c) == Some(r.as_str()))
                })
                .collect();
            let district = json!({"field": "city_code", "op": "in", "value": codes});
            Some(if sido_wide.is_empty() {
                district
            } else {
                json!({"any": [{"field": "region_code", "op": "in", "value": sido_wide}, district]})
            })
        }
        "S-enrollment" => {
            let listed = columns
                .enrollment_statuses
                .as_ref()
                .filter(|s| !s.is_empty())?;
            // Profiles carry canonical statuses only, so aliases admit nobody.
            let admitted: Vec<&String> = listed
                .iter()
                .filter(|s| ENROLLMENT_STATUSES.contains(&s.as_str()))
                .collect();
            Some(json!({"field": "enrollment_status", "op": "in", "value": admitted}))
        }
        _ => {
            let found = matching_conditions(columns.hard_filter.as_ref(), target);
            match found.as_slice() {
                [] => None,
                [one] => Some((*one).clone()),
                many => Some(json!({ "all": many })),
            }
        }
    }
}

// ── Containment ────────────────────────────────────────────────────────────

/// Whether `wide` provably admits everyone `narrow` admits: wherever
/// `narrow` is not `Fail`, `wide` is not `Fail` either. Sufficient, not
/// necessary: what cannot be shown is `false`.
pub fn contains(wide: &Value, narrow: &Value) -> bool {
    if let Some(children) = narrow.get("any").and_then(Value::as_array) {
        return !children.is_empty() && children.iter().all(|c| contains(wide, c));
    }
    if let Some(children) = narrow.get("all").and_then(Value::as_array) {
        return children.iter().any(|c| contains(wide, c));
    }
    if let Some(children) = wide.get("any").and_then(Value::as_array) {
        return children.iter().any(|c| contains(c, narrow));
    }
    if let Some(children) = wide.get("all").and_then(Value::as_array) {
        return !children.is_empty() && children.iter().all(|c| contains(c, narrow));
    }
    leaf_contains(wide, narrow)
}

fn is_label(leaf: &Value) -> bool {
    leaf.get("severity").and_then(Value::as_str) == Some("label")
}

fn canonical_field(field: &str) -> &str {
    if field == "region" {
        "region_code"
    } else {
        field
    }
}

fn leaf_contains(wide: &Value, narrow: &Value) -> bool {
    // A label leaf never fails: it admits everyone. Nothing hard contains it.
    if is_label(wide) {
        return true;
    }
    if is_label(narrow) {
        return false;
    }
    fn field(leaf: &Value) -> Option<&str> {
        leaf.get("field")
            .and_then(Value::as_str)
            .map(canonical_field)
    }
    let (Some(wf), Some(nf)) = (field(wide), field(narrow)) else {
        return false;
    };
    match (wf, nf) {
        ("city_code", "city_code") => match (codes(wide), codes(narrow)) {
            // The district rule of the structural check, never a prefix.
            (Some(w), Some(n)) => n.iter().all(|n| {
                w.iter()
                    .any(|w| structural::district_covers(w, n) == Some(true))
            }),
            _ => false,
        },
        // A covered district lies in its sido.
        ("region_code", "city_code") => match (StrSet::of(wide), codes(narrow)) {
            (Some(w), Some(n)) => n
                .iter()
                .all(|code| structural::district_region(code).is_some_and(|sido| w.admits(sido))),
            _ => false,
        },
        // An unresolved region scope admits nobody: an explicit list of
        // regions is wider. An all-but set is not accepted for it.
        ("region_code", "region_code") if unresolved_scope(narrow) => {
            StrSet::of(wide).is_some_and(|w| w.listed)
        }
        (w, n) if w == n => {
            if let (Some(w), Some(n)) = (IntSet::of(wide), IntSet::of(narrow)) {
                return w.contains(&n);
            }
            if let (Some(w), Some(n)) = (StrSet::of(wide), StrSet::of(narrow)) {
                return w.contains(&n);
            }
            match (op(wide), op(narrow)) {
                (Some(a @ ("is_true" | "is_false")), Some(b)) => a == b,
                _ => false,
            }
        }
        _ => false,
    }
}

/// A region target whose scope is unresolved (`regions` holds "unknown").
pub fn unresolved_scope(target: &Value) -> bool {
    canonical_field(
        target
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) == "region_code"
        && target
            .get("value")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|v| v == "unknown"))
}

fn op(leaf: &Value) -> Option<&str> {
    leaf.get("op").and_then(Value::as_str)
}

/// Canonical district codes of an `in`/`eq` `city_code` leaf.
fn codes(leaf: &Value) -> Option<Vec<String>> {
    let raw: Vec<&Value> = match (op(leaf)?, leaf.get("value")?) {
        ("in", Value::Array(items)) => items.iter().collect(),
        ("eq", value) => vec![value],
        _ => return None,
    };
    raw.into_iter()
        .map(|v| v.as_str().and_then(structural::district_code))
        .collect()
}

/// The integers an integer leaf admits.
#[derive(Debug)]
enum IntSet {
    Range(Option<i64>, Option<i64>),
    Some(Vec<i64>),
    AllBut(Vec<i64>),
}

impl IntSet {
    fn of(leaf: &Value) -> Option<Self> {
        let value = leaf.get("value")?;
        let int = |v: &Value| v.as_i64();
        let ints = |v: &Value| -> Option<Vec<i64>> { v.as_array()?.iter().map(int).collect() };
        Some(match op(leaf)? {
            "eq" => Self::Some(vec![int(value)?]),
            "in" => Self::Some(ints(value)?),
            "ne" => Self::AllBut(vec![int(value)?]),
            "not_in" => Self::AllBut(ints(value)?),
            "between" => match ints(value)?.as_slice() {
                [lo, hi] => Self::Range(Some(*lo), Some(*hi)),
                _ => return None,
            },
            "gte" => Self::Range(Some(int(value)?), None),
            "gt" => Self::Range(Some(int(value)?.checked_add(1)?), None),
            "lte" => Self::Range(None, Some(int(value)?)),
            "lt" => Self::Range(None, Some(int(value)?.checked_sub(1)?)),
            _ => return None,
        })
    }

    fn admits(&self, n: i64) -> bool {
        match self {
            Self::Range(lo, hi) => lo.is_none_or(|lo| n >= lo) && hi.is_none_or(|hi| n <= hi),
            Self::Some(values) => values.contains(&n),
            Self::AllBut(values) => !values.contains(&n),
        }
    }

    fn contains(&self, narrow: &Self) -> bool {
        match (self, narrow) {
            (_, Self::Some(values)) => values.iter().all(|n| self.admits(*n)),
            (Self::Range(wlo, whi), Self::Range(nlo, nhi)) => {
                let lower = match (wlo, nlo) {
                    (None, _) => true,
                    (Some(w), Some(n)) => w <= n,
                    (Some(_), None) => false,
                };
                let upper = match (whi, nhi) {
                    (None, _) => true,
                    (Some(w), Some(n)) => w >= n,
                    (Some(_), None) => false,
                };
                // An empty narrow range admits nobody.
                lower && upper || nlo.zip(*nhi).is_some_and(|(lo, hi)| lo > hi)
            }
            (Self::Some(values), Self::Range(Some(lo), Some(hi))) => {
                hi - lo <= 1000 && (*lo..=*hi).all(|n| values.contains(&n))
            }
            (Self::AllBut(excluded), Self::Range(lo, hi)) => {
                excluded.iter().all(|n| !Self::Range(*lo, *hi).admits(*n))
            }
            (Self::AllBut(w), Self::AllBut(n)) => w.iter().all(|x| n.contains(x)),
            _ => false,
        }
    }
}

/// The strings an enum or text leaf admits: listed values, or all but them.
#[derive(Debug)]
struct StrSet {
    listed: bool,
    values: Vec<String>,
}

impl StrSet {
    fn of(leaf: &Value) -> Option<Self> {
        let value = leaf.get("value")?;
        let strings = |v: &Value| -> Option<Vec<String>> {
            v.as_array()?
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect()
        };
        let (listed, values) = match op(leaf)? {
            "eq" => (true, vec![value.as_str()?.to_owned()]),
            "in" => (true, strings(value)?),
            "ne" => (false, vec![value.as_str()?.to_owned()]),
            "not_in" => (false, strings(value)?),
            _ => return None,
        };
        Some(Self { listed, values })
    }

    fn admits(&self, value: &str) -> bool {
        self.values.iter().any(|v| v == value) == self.listed
    }

    fn contains(&self, narrow: &Self) -> bool {
        match (self.listed, narrow.listed) {
            (_, true) => narrow.values.iter().all(|v| self.admits(v)),
            (false, false) => self.values.iter().all(|v| narrow.values.contains(v)),
            (true, false) => false,
        }
    }
}

// ── Serve ──────────────────────────────────────────────────────────────────

/// The overrides that apply to a programme now: the target exists and a
/// `widen` replacement still contains it. Others are ignored (the code
/// rule hides as before), e.g. after a mapping fix narrowed the column.
pub fn applicable<'a>(
    overrides: &'a [CodeOverride],
    columns: &CodeColumns,
) -> Vec<&'a CodeOverride> {
    overrides
        .iter()
        .filter(|o| match target_rule(&o.target, columns) {
            None => false,
            Some(target) => match (o.action, &o.replacement) {
                // An unresolved scope is only ever widened to an explicit list.
                (Action::Drop, _) => !unresolved_scope(&target),
                (Action::Widen, Some(replacement)) => contains(replacement, &target),
                (Action::Widen, None) => false,
            },
        })
        .collect()
}

/// Structural checks of a row with its applied `S-` overrides: an
/// overridden check is left out and its replacement (if any) is evaluated
/// instead. `Err` = hidden, with the reason the filter summary counts.
/// `Ok` carries the remaining checks and the replacements' evaluations.
/// `today` is the caller's Korean date (the request clock), so the checks
/// and the replacements count 만 나이 on the days the caller's own rules do
/// (review2-verify gap 4).
pub fn structural_gate<P: EligibilityProfile>(
    program: &Program,
    program_university_id: Option<&str>,
    structural_profile: &StructuralProfile,
    profile: &P,
    overrides: &[&CodeOverride],
    today: chrono::NaiveDate,
) -> Result<(Vec<StructuralCheck>, Vec<RuleEvalResult>), ExclusionReason> {
    let basis = rule_engine::AgeBasis::for_program(program, today);
    let mut checks =
        structural::structural_checks_at(program, program_university_id, structural_profile, today);
    checks.retain(|c| {
        !overrides
            .iter()
            .any(|o| o.structural_kind() == Some(c.kind))
    });
    if let Some(reason) = structural::exclusion(&checks) {
        return Err(reason);
    }
    let mut evaluations = Vec::new();
    for o in overrides.iter().filter(|o| o.structural_kind().is_some()) {
        if let Some(replacement) = &o.replacement {
            let evaluation = evaluate_replacement(profile, replacement, &basis);
            if evaluation.verdict == RuleVerdict::Fail {
                return Err(o.exclusion());
            }
            evaluations.push(evaluation);
        }
    }
    Ok((checks, evaluations))
}

/// The applied overrides this profile is listed through: the code rule
/// each one replaces would hide the profile. For an `S-` target that is the
/// structural check the gate leaves out (`Fail`, or an unresolved scope);
/// for a `C-` target, the source conditions of the active hard filter
/// (`columns.hard_filter`, before stripping) evaluate to `Fail`. A profile
/// the code rule passes or leaves unknown is not admitted by the override,
/// it only gains the override's check. `today` as in [`structural_gate`].
pub fn admitting<'a, P: EligibilityProfile>(
    program: &Program,
    program_university_id: Option<&str>,
    structural_profile: &StructuralProfile,
    profile: &P,
    overrides: &[&'a CodeOverride],
    columns: &CodeColumns,
    today: chrono::NaiveDate,
) -> Vec<&'a CodeOverride> {
    if overrides.is_empty() {
        return Vec::new();
    }
    let basis = rule_engine::AgeBasis::for_program(program, today);
    let checks =
        structural::structural_checks_at(program, program_university_id, structural_profile, today);
    overrides
        .iter()
        .copied()
        .filter(|o| match o.structural_kind() {
            Some(kind) => checks
                .iter()
                .any(|c| c.kind == kind && (c.status == RuleVerdict::Fail || c.unresolved_scope)),
            None => target_rule(&o.target, columns)
                .and_then(|target| rule_engine::parse_rule_json(&target).ok())
                .is_some_and(|node| {
                    rule_engine::evaluate(profile, &node, &basis).verdict == RuleVerdict::Fail
                }),
        })
        .collect()
}

/// `tree` without the top-level source conditions the `C-` overrides
/// target; `None` when nothing is left.
pub fn strip_targets(tree: Option<&Value>, overrides: &[&CodeOverride]) -> Option<Value> {
    let targeted: Vec<&Value> = overrides
        .iter()
        .flat_map(|o| matching_conditions(tree, &o.target))
        .collect();
    let kept: Vec<&Value> = rule_engine::top_level_conditions(tree)
        .into_iter()
        .filter(|c| !targeted.iter().any(|t| std::ptr::eq(*t, *c)))
        .collect();
    (!kept.is_empty()).then(|| json!({ "all": kept }))
}

/// Evaluate a replacement as a hard rule. A `city_code` leaf means district
/// coverage exactly as [`structural::district_check`] reads it (a 시 covers
/// its 구; a 시 answer for a 구 programme is unknown; a user whose sido has
/// no listed district fails), so a replacement that lists the target's
/// districts never fails where the target would not.
pub fn evaluate_replacement<P: EligibilityProfile + ?Sized>(
    profile: &P,
    node: &Value,
    basis: &rule_engine::AgeBasis,
) -> RuleEvalResult {
    let combine = |children: Vec<RuleEvalResult>, any: bool| -> RuleEvalResult {
        let verdicts: Vec<RuleVerdict> = children.iter().map(|c| c.verdict).collect();
        let verdict = if any {
            if verdicts.contains(&RuleVerdict::Pass) {
                RuleVerdict::Pass
            } else if verdicts.contains(&RuleVerdict::Unknown) {
                RuleVerdict::Unknown
            } else {
                RuleVerdict::Fail
            }
        } else if verdicts.contains(&RuleVerdict::Fail) {
            RuleVerdict::Fail
        } else if verdicts.contains(&RuleVerdict::Unknown) {
            RuleVerdict::Unknown
        } else {
            RuleVerdict::Pass
        };
        let mut out = result(verdict, Vec::new(), Vec::new(), Vec::new());
        for child in children {
            out.matched_rules.extend(child.matched_rules);
            out.failed_rules.extend(child.failed_rules);
            out.unknown_rules.extend(child.unknown_rules);
        }
        out
    };
    if let Some(children) = node.get("any").and_then(Value::as_array) {
        return combine(
            children
                .iter()
                .map(|c| evaluate_replacement(profile, c, basis))
                .collect(),
            true,
        );
    }
    if let Some(children) = node.get("all").and_then(Value::as_array) {
        return combine(
            children
                .iter()
                .map(|c| evaluate_replacement(profile, c, basis))
                .collect(),
            false,
        );
    }
    if node.get("field").and_then(Value::as_str) == Some("city_code") {
        return district_leaf(profile, node);
    }
    match rule_engine::parse_rule_json(node) {
        Ok(leaf) => rule_engine::evaluate(profile, &leaf, basis),
        // Validated on write; a malformed stored leaf decides nothing.
        Err(_) => result(RuleVerdict::Unknown, Vec::new(), Vec::new(), Vec::new()),
    }
}

fn result(
    verdict: RuleVerdict,
    matched: Vec<EvalDetail>,
    failed: Vec<EvalDetail>,
    unknown: Vec<EvalDetail>,
) -> RuleEvalResult {
    RuleEvalResult {
        verdict,
        matched_rules: matched,
        failed_rules: failed,
        unknown_rules: unknown,
        match_percentage: 0.0,
        explain_reasons: Vec::new(),
    }
}

fn district_leaf<P: EligibilityProfile + ?Sized>(profile: &P, leaf: &Value) -> RuleEvalResult {
    let listed = codes(leaf).unwrap_or_default();
    let region = profile
        .eligibility_value("region_code")
        .and_then(|v| v.as_str().and_then(crate::canonical_region_code));
    let user = profile
        .eligibility_value("city_code")
        .and_then(|v| match structural::current_district(v.as_str()?) {
            structural::Current::Current(code) => Some(code),
            _ => None,
        })
        .filter(|code| region.is_some() && structural::district_region(code) == region);
    let mut detail = EvalDetail {
        field: "city_code".into(),
        op: "in".into(),
        expected: Value::from(listed.clone()).to_string(),
        actual: user.clone(),
        weight: 1.0,
        passed: None,
    };
    let Some(region) = region else {
        return result(RuleVerdict::Unknown, Vec::new(), Vec::new(), vec![detail]);
    };
    let mine: Vec<&String> = listed
        .iter()
        .filter(|c| structural::district_region(c) == Some(region))
        .collect();
    if mine.is_empty() {
        detail.passed = Some(false);
        return result(RuleVerdict::Fail, Vec::new(), vec![detail], Vec::new());
    }
    let Some(user) = user else {
        return result(RuleVerdict::Unknown, Vec::new(), Vec::new(), vec![detail]);
    };
    let found: Vec<Option<bool>> = mine
        .iter()
        .map(|code| structural::district_covers(code, &user))
        .collect();
    if found.contains(&Some(true)) {
        detail.passed = Some(true);
        result(RuleVerdict::Pass, vec![detail], Vec::new(), Vec::new())
    } else if found.contains(&None) {
        result(RuleVerdict::Unknown, Vec::new(), Vec::new(), vec![detail])
    } else {
        detail.passed = Some(false);
        result(RuleVerdict::Fail, Vec::new(), vec![detail], Vec::new())
    }
}

/// A replacement's leaves and any-groups, for vocabulary checks.
pub fn replacement_leaves(node: &Value) -> Vec<&Map<String, Value>> {
    match node.get("any").and_then(Value::as_array) {
        Some(children) => children.iter().filter_map(Value::as_object).collect(),
        None => node.as_object().into_iter().collect(),
    }
}
