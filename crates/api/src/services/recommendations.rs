use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;

use mazimi_core::applicant_review::{ReviewEffect, INDIRECT_NOTE};
use mazimi_core::audience::{self, AudienceExclusion, AudienceProfile, AudienceText, Persona};
use mazimi_core::condition_review::overrides::{self, CodeColumns, CodeOverride};
use mazimi_core::condition_review::{self, Assessment, ServeFacts};
use mazimi_core::models::{
    ConditionCounts, EligibilityRule, ExclusionReason, FilterSummary, ProfileInput, Program,
    RecommendScope, RecommendationItem, RecommendationResult, UserProfile,
    UNVERIFIED_CONDITIONS_LABEL,
};
use mazimi_core::rule_engine::{self, RankSignals, RuleEvalResult, RuleVerdict};
use mazimi_core::structural::{self, CheckKind, DistrictMatch, StructuralProfile};
use mazimi_core::youth_fit;

use super::condition_reviews::ReviewFacts;

/// POST /api/v1/recommend/preview
/// Returns recommendations based on profile input without requiring login.
pub async fn preview(
    pool: &PgPool,
    input: ProfileInput,
    scope: RecommendScope,
) -> Result<RecommendationResult, (StatusCode, Json<Value>)> {
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let result = preview_on(&mut tx, input, scope).await?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(result)
}

pub(super) async fn preview_on(
    connection: &mut sqlx::PgConnection,
    input: ProfileInput,
    scope: RecommendScope,
) -> Result<RecommendationResult, (StatusCode, Json<Value>)> {
    preview_with(connection, input, scope, &[], &[]).await
}

/// [`preview_on`] that also lists demoted rows of the `requested` youth_fit
/// targets (an assistant question that clearly asks for them), with the
/// profile fields the question `assumed` ([`evaluate_assumed`]).
pub(super) async fn preview_with(
    connection: &mut sqlx::PgConnection,
    mut input: ProfileInput,
    scope: RecommendScope,
    requested: &[youth_fit::Target],
    assumed: &[&str],
) -> Result<RecommendationResult, (StatusCode, Json<Value>)> {
    let now = Utc::now();
    super::profile_validation::normalize_profile(&mut input)?;

    // A university-only offer is not a nationwide offer simply because it
    // carries no geographic restriction. Resolve the selected school against
    // the same catalogue used by the campus tab; arbitrary text unlocks none.
    let selected_school = match input.school_name.as_deref() {
        Some(school) => {
            super::profile_validation::resolve_university(&mut *connection, school).await?
        }
        None => None,
    };
    if let Some((_, name)) = &selected_school {
        input.school_name = Some(name.clone());
    }

    // The published catalogue, flagged with whether the user can act on it
    // at this request's clock. Every row is decided once by `evaluate`
    // (structural checks shared with the assistant's explanation, then the
    // hard filter), so the filter summary counts exactly what the list hides.
    // Only decision columns are read here; display text is loaded below for
    // the listed rows alone.
    //
    // Exclude-only narrowing (review 2026-10-21 M21): a row limited to other
    // sidos is hidden by the structural gate whatever else it says, unless an
    // `S-` code override replaces one of its checks. Such rows are only
    // counted in SQL by the reason a full read gives them (below), never
    // read with notice text, so the list and its summary stay what a full
    // read gives.
    let region = mazimi_core::canonical_region_code(&input.region_code);
    let elsewhere = format!("COALESCE(({ELSEWHERE_SQL}), false)");
    let available = mazimi_core::currently_available_sql("", "$1");
    let catalogue = sqlx::query_as::<_, CatalogueRow>(&format!(
        r#"
        SELECT {decision}, {raw} AS raw_payload, university_id,
               ({available}) AS currently_available, {AUDIENCE_COLUMNS}, {YOUTH_FIT_COLUMNS},
               {categories}
        FROM current_programs
        WHERE NOT {elsewhere}
        ORDER BY LEAST(benefit_valid_until, deadline_at, application_end_at) ASC NULLS LAST, id ASC
        "#,
        decision = decision_columns(),
        raw = super::evidence::LIST_RAW_PAYLOAD_SQL,
        categories = mazimi_core::categories::columns_sql("current_programs"),
    ))
    .bind(now)
    .bind(region)
    .fetch_all(&mut *connection)
    .await
    .map_err(crate::errors::internal_error)?;
    // Rows of other sidos are only counted, by the reason a full read gives
    // them: not open first, then a failed age check under every reading of
    // the age ([`rule_engine::age_gate_for`]: one with a date of birth, both
    // readings of a bare birth year), else their region. Counted on today:
    // these rows are never shown, so their notice windows are not read.
    let ages = rule_engine::ages(
        input.birth_year,
        input.birth_date,
        &rule_engine::AgeBasis::on(mazimi_core::seoul_date(now)),
    );
    let (younger, older) = ages;
    let outside = |age: &str| {
        format!(
            "((min_age IS NOT NULL AND {age} < min_age) OR (max_age IS NOT NULL AND {age} > max_age))"
        )
    };
    let age_fails = format!("({} AND {})", outside("$3"), outside("$4"));
    let (elsewhere_not_open, elsewhere_age, elsewhere_region) =
        sqlx::query_as::<_, (i64, i64, i64)>(&format!(
            "SELECT count(*) FILTER (WHERE NOT ({available})),
                    count(*) FILTER (WHERE ({available}) AND {age_fails}),
                    count(*) FILTER (WHERE ({available}) AND NOT {age_fails})
             FROM current_programs WHERE {elsewhere}"
        ))
        .bind(now)
        .bind(region)
        .bind(younger)
        .bind(older)
        .fetch_one(&mut *connection)
        .await
        .map_err(crate::errors::internal_error)?;
    // Reviewed applicant classes, read in the same snapshot. Applied after
    // the text rules, so a review only hides or demotes what they list.
    let reviewed = super::applicant_reviews::effects(&mut *connection, None)
        .await
        .map_err(crate::errors::internal_error)?;
    // Reviewed widenings of code rules (current reviews only), so an
    // overridden age, region, district or enrollment rule does not hide
    // the row in the structural pass below. `evaluate_ranked` applies the
    // same overrides from the review it loads.
    // Only the rows read above can be listed; overrides of the others are
    // not needed (their `S-` overrides kept them in that read).
    let catalogue_ids: Vec<Uuid> = catalogue.iter().map(|row| row.program.id).collect();
    let code_overrides =
        super::condition_reviews::load_overrides(&mut *connection, Some(&catalogue_ids))
            .await
            .map_err(crate::errors::internal_error)?;
    let school_id = selected_school.as_ref().map(|(id, _)| id.as_str());
    let profile = structural_profile(&input, school_id);
    let audience_profile = audience_profile(&input, mazimi_core::seoul_year(now));
    let mut decisions: Vec<Result<bool, ExclusionReason>> = Vec::with_capacity(catalogue.len());
    for (reason, count) in [
        (ExclusionReason::NotOpen, elsewhere_not_open),
        (ExclusionReason::Age, elsewhere_age),
        (ExclusionReason::Region, elsewhere_region),
    ] {
        decisions.extend(std::iter::repeat_n(Err(reason), count as usize));
    }
    let mut programs: Vec<(&CatalogueRow, bool, audience::Membership)> = Vec::new();
    for row in &catalogue {
        if !row.currently_available {
            decisions.push(Err(ExclusionReason::NotOpen));
            continue;
        }
        // Same structural gate `evaluate` starts with; checked here too so
        // rules are only loaded for rows it does not already exclude. Only
        // `S-` overrides matter here; they read the programme's columns.
        let applied = code_overrides
            .get(&row.program.id)
            .map(|list| overrides::applicable(list, &CodeColumns::of(&row.program, None)))
            .unwrap_or_default();
        if let Err(reason) = overrides::structural_gate(
            &row.program,
            row.university_id.as_deref(),
            &profile,
            &input,
            &applied,
            mazimi_core::seoul_date(now),
        ) {
            decisions.push(Err(reason));
            continue;
        }
        let facts = row.facts(reviewed.get(&row.program.id).copied());
        match row_gates(&facts, &input, &audience_profile, scope, requested) {
            Err(reason) => decisions.push(Err(reason)),
            Ok((youth, membership)) => programs.push((row, youth, membership)),
        }
    }

    // Batch-load all eligibility_rules for the remaining programs in one
    // query. Keyed by program_id; a program may have at most one active rule row.
    let program_ids: Vec<Uuid> = programs.iter().map(|(p, ..)| p.program.id).collect();

    let eligibility_rules: HashMap<Uuid, EligibilityRule> = if program_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, EligibilityRule>(
            r#"
            SELECT DISTINCT ON (program_id) *
            FROM eligibility_rules
            WHERE program_id = ANY($1)
            ORDER BY program_id, version DESC
            "#,
        )
        .bind(&program_ids)
        .fetch_all(&mut *connection)
        .await
        .map_err(crate::errors::internal_error)?
        .into_iter()
        .map(|r| (r.program_id, r))
        .collect()
    };

    // Condition and applicant reviews of the remaining rows, in the same
    // snapshot (plan "verified conditions" §1.1).
    let review_facts = super::condition_reviews::load(&mut *connection, &program_ids)
        .await
        .map_err(crate::errors::internal_error)?;

    let section_enabled = verified_section_enabled();
    let persona = persona(&input);
    let mut items: Vec<(f64, RecommendationItem)> = Vec::new();
    let mut listed: Vec<(&Program, Option<&Value>)> = Vec::new();
    // Reviewed `indirect`: an institution applies, the user takes part
    // through it.
    let mut indirect: HashSet<Uuid> = HashSet::new();
    for (row, youth, membership) in &programs {
        let rule = eligibility_rules.get(&row.program.id);
        let relevance = Relevance {
            youth: *youth,
            youth_title: audience::youth_title(&row.program.title),
            persona: audience::persona_match(
                &persona,
                &row.program.program_type,
                &row.program.title,
            ),
        };
        let decision = evaluate_assumed(
            &row.program,
            row.university_id.as_deref(),
            &input,
            school_id,
            rule,
            membership,
            relevance,
            ReviewContext {
                facts: review_facts.get(&row.program.id),
                now,
                section_enabled,
            },
            assumed,
        );
        // Confirmed in the summary = what a client may rest on (audit M1).
        decisions.push(
            decision
                .as_ref()
                .map(|(_, item)| evidence_level(item) == EvidenceLevel::Verified)
                .map_err(|reason| *reason),
        );
        if let Ok(mut item) = decision {
            item.1.categories = row.categories.clone().into();
            if reviewed.get(&row.program.id) == Some(&ReviewEffect::Demote) {
                // A reviewed row may already carry the note from its
                // remaining checks; it is listed once, first.
                item.1.missing_checks.retain(|check| check != INDIRECT_NOTE);
                item.1.missing_checks.insert(0, INDIRECT_NOTE.to_owned());
                indirect.insert(row.program.id);
            }
            listed.push((&row.program, rule.and_then(|r| r.hard_filter_json.as_ref())));
            items.push(item);
        }
    }
    let filter_summary = FilterSummary::tally(decisions);

    let mut evidence = super::evidence::load(&mut *connection, &listed)
        .await
        .map_err(crate::errors::internal_error)?;
    let listed_ids: Vec<Uuid> = items.iter().map(|(_, item)| item.program_id).collect();
    // Display text of the listed rows only (the decision read carries none).
    let mut guides: HashMap<Uuid, GuideText> = sqlx::query_as::<_, GuideText>(
        "SELECT id, application_method, submission_documents, screening_method, summary
         FROM programs WHERE id = ANY($1)",
    )
    .bind(&listed_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(crate::errors::internal_error)?
    .into_iter()
    .map(|guide| (guide.0, guide))
    .collect();
    for (_, item) in &mut items {
        if let Some((_, method, documents, screening, summary)) = guides.remove(&item.program_id) {
            item.application_method = method;
            item.submission_documents = documents;
            item.screening_method = screening;
            // The card body (audit M6).
            item.summary_display = card_summary(summary.as_deref());
        }
        item.evidence = evidence
            .remove(&item.program_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.check.is_eligibility())
            .collect();
    }

    rank_items(&mut items, &indirect, section_enabled);

    let items: Vec<RecommendationItem> = items.into_iter().map(|(_, item)| item).collect();

    // A corporate offer's amounts are no support money (pays_support). The
    // sums add only "조건 확인됨" rows: a total over rows left 확인 필요 would
    // claim money the user may not get (audit L8, FACT-20261007-002).
    let paid = |i: &&RecommendationItem| {
        mazimi_core::pays_support(&i.program_type) && evidence_level(i) == EvidenceLevel::Verified
    };
    let estimated_monthly: i64 = items
        .iter()
        .filter(paid)
        .filter_map(|i| i.benefit_amount_monthly)
        .map(|v| v as i64)
        .sum();

    let estimated_semester: i64 = items
        .iter()
        .filter(paid)
        .filter_map(|i| i.benefit_amount_semester)
        .map(|v| v as i64)
        .sum();

    let total = items.len();
    let verified_count = items.iter().filter(|item| item.verified).count();

    Ok(RecommendationResult {
        total_available: total,
        estimated_monthly,
        estimated_semester,
        items,
        filter_summary,
        scope,
        verified_count,
        verified_section_enabled: section_enabled,
    })
}

/// `VERIFIED_SECTION_ENABLED` (default off): whether clients may render the
/// "조건 확인됨 / 확인 필요" sections. Read once per request. While off,
/// `verified` is served `false` and `verified_count` 0 (review 2026-10-21
/// M2): `verified` is the only eligibility claim a client may rest on.
/// `remaining_checks` and `window` are computed either way.
pub(crate) fn verified_section_enabled() -> bool {
    std::env::var("VERIFIED_SECTION_ENABLED").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )
    })
}

/// Rows listed only because a code override admitted the user (an
/// [`overrides::EXCEPTION_KIND`] check) after every other row. Then
/// "조건 확인됨" rows first (only while the section is enabled), then by
/// [`evidence_tier`]: a notice rule passed with every condition, a notice
/// rule passed with one left open, a pass on the source's codes alone, then
/// rows unchecked or left unknown; within a
/// tier rows a person applies for directly before reviewed
/// `indirect` rows, then by score. The sort is stable, so ties keep the
/// catalogue's deadline order.
pub(crate) fn rank_items(
    items: &mut [(f64, RecommendationItem)],
    indirect: &HashSet<Uuid>,
    section_enabled: bool,
) {
    let first = |item: &RecommendationItem| section_enabled && item.verified;
    let exception = |item: &RecommendationItem| {
        item.remaining_checks
            .iter()
            .any(|check| check.kind == overrides::EXCEPTION_KIND)
    };
    items.sort_by(|a, b| {
        exception(&a.1)
            .cmp(&exception(&b.1))
            .then_with(|| first(&b.1).cmp(&first(&a.1)))
            .then_with(|| evidence_tier(&b.1).cmp(&evidence_tier(&a.1)))
            .then_with(|| {
                indirect
                    .contains(&a.1.program_id)
                    .cmp(&indirect.contains(&b.1.program_id))
            })
            .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });
}

/// A corporate offer the worker published from its official page
/// (`normalized_payload.display = 'verbatim'`): always 확인 필요.
fn auto_published_offer(prog: &Program) -> bool {
    prog.program_type == "corporate_benefit"
        && prog
            .normalized_payload
            .as_ref()
            .and_then(|p| p.get("display"))
            .and_then(|d| d.as_str())
            == Some("verbatim")
}

/// What a listed row's eligibility rests on, best first (audit M1). Read
/// from the item once it is final, so the ranking and the filter summary
/// take the same reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceLevel {
    /// "조건 확인됨": the reviewed conjunction passed (section enabled).
    Verified,
    /// A rule read from the notice was evaluated and passed.
    NoticeRulePassed,
    /// `pass` on the source's own codes only: nothing a reviewer read in
    /// the notice was checked.
    CodesOnly,
    /// Shown on trust or left unknown.
    Unknown,
}

pub(crate) fn evidence_level(item: &RecommendationItem) -> EvidenceLevel {
    if item.verified {
        EvidenceLevel::Verified
    } else {
        rule_level(item)
    }
}

/// [`evidence_level`] without the served `verified` flag: what the rules
/// alone say. The ranking reads this; "조건 확인됨" rows rank first on their
/// own key, only while the section is enabled ([`rank_items`]).
fn rule_level(item: &RecommendationItem) -> EvidenceLevel {
    if item.conditions_verified {
        EvidenceLevel::NoticeRulePassed
    } else if item.eligibility_status == "pass" {
        EvidenceLevel::CodesOnly
    } else {
        EvidenceLevel::Unknown
    }
}

/// 3 = a notice rule passed and every condition passed, 2 = a notice rule
/// passed with a condition left open, 1 = passed on the source's codes
/// alone, 0 = shown on trust or left unknown.
fn evidence_tier(item: &RecommendationItem) -> u8 {
    match rule_level(item) {
        EvidenceLevel::NoticeRulePassed if item.eligibility_status == "pass" => 3,
        EvidenceLevel::NoticeRulePassed | EvidenceLevel::Verified => 2,
        EvidenceLevel::CodesOnly => 1,
        EvidenceLevel::Unknown => 0,
    }
}

/// Target text the audience classifier reads, cut to a bounded size so a
/// request never pulls whole notices. See [`mazimi_core::audience`].
const AUDIENCE_COLUMNS: &str = "\
    CASE source_type \
      WHEN 'gov_benefits' THEN left(concat_ws(E'\\n', raw_payload->>'지원대상', raw_payload->>'선정기준'), 2000) \
      WHEN 'youth_center' THEN left(concat_ws(E'\\n', raw_payload->>'addAplyQlfcCndCn', raw_payload->>'plcyAplyMthdCn'), 2000) \
    END AS audience_target, \
    CASE WHEN source_type = 'gov_benefits' THEN raw_payload->>'사용자구분' END AS applicant_kinds, \
    CASE WHEN source_type IN ('local_welfare', 'national_welfare') \
      THEN COALESCE(raw_payload->>'trgterIndvdlNmArray', raw_payload->>'trgterIndvdlArray') END AS target_tags, \
    CASE WHEN source_type IN ('local_welfare', 'national_welfare') \
      THEN COALESCE(raw_payload->>'lifeNmArray', raw_payload->>'lifeArray') END AS life_stages, \
    left(summary, 600) AS audience_summary, \
    COALESCE(normalized_payload->'youth_relevant' = 'true'::jsonb, false) AS curated_youth";

/// The stored youth_fit verdict and target (a reviewed verdict wins).
const YOUTH_FIT_COLUMNS: &str = "\
    (SELECT COALESCE(yf.reviewed_verdict, yf.verdict) FROM program_youth_fit yf \
      WHERE yf.program_id = current_programs.id) AS youth_fit, \
    (SELECT CASE WHEN yf.reviewed_verdict IS NULL THEN yf.target ELSE yf.reviewed_target END \
      FROM program_youth_fit yf WHERE yf.program_id = current_programs.id) AS youth_fit_target";

/// Hard audience exclusions (any scope), the membership of who the row is
/// reserved for (`tier` = its stored youth_fit verdict), then the youth
/// scope. `Ok((youth, membership))`: listed; [`evaluate_ranked`] takes the
/// membership's checks with the structural ones, so a reserved row's status
/// never exceeds them.
fn audience_gate(
    text: &AudienceText,
    holder: Option<mazimi_core::holder::Holder>,
    holder_unreadable: bool,
    tier: Option<youth_fit::Tier>,
    profile: &AudienceProfile,
    scope: RecommendScope,
) -> Result<(bool, audience::Membership), ExclusionReason> {
    let mut audience = audience::classify(text);
    audience.holder = holder;
    audience.holder_unreadable = holder_unreadable;
    match audience::hard_exclusion(&audience, profile) {
        Some(AudienceExclusion::NotIndividual) => return Err(ExclusionReason::NotIndividual),
        Some(AudienceExclusion::TargetGroup(_)) => return Err(ExclusionReason::TargetGroup),
        None => {}
    }
    let membership = audience::membership(&audience, tier, profile);
    if membership.hides() {
        return Err(ExclusionReason::TargetGroup);
    }
    if scope == RecommendScope::Youth && !audience.youth {
        return Err(ExclusionReason::OutsideYouthScope);
    }
    Ok((audience.youth, membership))
}

/// What the list reads about one row besides its programme columns, rule
/// and condition reviews.
pub(crate) struct RowFacts<'a> {
    pub audience: AudienceText<'a>,
    /// The stored youth_fit verdict and target ([`YOUTH_FIT_COLUMNS`]).
    pub youth_fit: Option<&'a str>,
    pub youth_fit_target: Option<&'a str>,
    /// The reviewed applicant class.
    pub applicant: Option<ReviewEffect>,
    /// What the user must hold (the row's `holder_requirement`).
    pub holder: Option<mazimi_core::holder::Holder>,
    /// The row's `holder_requirement` is set but unreadable (code review M-11).
    pub holder_unreadable: bool,
}

impl RowFacts<'_> {
    /// The stored youth_fit verdict (a reviewed one wins).
    fn youth_fit_verdict(&self) -> Option<youth_fit::Verdict> {
        self.tier().map(|t| t.verdict)
    }

    /// The stored youth_fit verdict with its target.
    fn tier(&self) -> Option<youth_fit::Tier> {
        self.youth_fit
            .and_then(|verdict| youth_fit::Tier::parse(verdict, self.youth_fit_target))
    }
}

/// The gates between the structural gate and the rule, in the list's
/// order: hard audience exclusions, the audience membership and the scope
/// ([`audience_gate`]), a reviewed applicant class that hides the row, the
/// youth_fit verdict. `Ok((youth, membership))`: the membership is passed
/// on to [`evaluate_ranked`], which reads its checks; it is never read a
/// second time.
fn row_gates(
    facts: &RowFacts,
    input: &ProfileInput,
    profile: &AudienceProfile,
    scope: RecommendScope,
    requested: &[youth_fit::Target],
) -> Result<(bool, audience::Membership), ExclusionReason> {
    let (youth, membership) = audience_gate(
        &facts.audience,
        facts.holder,
        facts.holder_unreadable,
        facts.tier(),
        profile,
        scope,
    )?;
    if facts.applicant == Some(ReviewEffect::Hide) {
        return Err(ExclusionReason::NotIndividual);
    }
    youth_fit_gate(facts, input, profile, requested)?;
    Ok((youth, membership))
}

/// Demoted rows (see [`youth_fit::recommendation_exclusion`]) leave the
/// list in every scope; a row without a stored verdict stays.
fn youth_fit_gate(
    facts: &RowFacts,
    input: &ProfileInput,
    profile: &AudienceProfile,
    requested: &[youth_fit::Target],
) -> Result<(), ExclusionReason> {
    let Some(verdict) = facts.youth_fit_verdict() else {
        return Ok(());
    };
    let target = facts.youth_fit_target.and_then(youth_fit::Target::parse);
    let gate = youth_fit_gate_profile(input, profile);
    match youth_fit::recommendation_exclusion(verdict, target, gate, requested) {
        None => Ok(()),
        Some(youth_fit::Exclusion::NotIndividual) => Err(ExclusionReason::NotIndividual),
        Some(youth_fit::Exclusion::TargetGroup) => Err(ExclusionReason::TargetGroup),
    }
}

/// One programme decided as the list decides it, for the assistant's
/// verdict on that programme: [`row_gates`] (every scope: being outside
/// the youth list is no ineligibility) and then [`evaluate_ranked`], after
/// the availability the list reads first. `Err`
/// = the list hides the row. The list itself runs the same two steps with
/// the rules loaded in between; when a row fails several gates the reason
/// can differ from the one the filter summary counts, never the outcome.
pub(crate) fn decide(
    prog: &Program,
    university_id: Option<&str>,
    input: &ProfileInput,
    school_id: Option<&str>,
    rule_row: Option<&EligibilityRule>,
    facts: &RowFacts,
    review: ReviewContext<'_>,
) -> Result<Decided, ExclusionReason> {
    // The list's first gate: a row it cannot act on now is not open.
    if !prog.is_currently_available(review.now) {
        return Err(ExclusionReason::NotOpen);
    }
    let profile = audience_profile(input, mazimi_core::seoul_year(review.now));
    let (youth, membership) = row_gates(facts, input, &profile, RecommendScope::All, &[])?;
    let (_, item) = evaluate_ranked(
        prog,
        university_id,
        input,
        school_id,
        rule_row,
        &membership,
        Relevance {
            youth,
            ..Relevance::default()
        },
        review,
    )?;
    let audience_open = membership
        .checks()
        .iter()
        .any(|c| c.status == RuleVerdict::Unknown);
    Ok(Decided {
        item,
        audience_open,
    })
}

/// A row [`decide`] lists, with whether its audience membership left a
/// check open (listed, never `pass`, "지원 대상" to confirm).
#[derive(Debug)]
pub(crate) struct Decided {
    pub item: RecommendationItem,
    pub audience_open: bool,
}

/// How the list decides one programme for one profile: the three outcomes
/// the card reads ([`decide_on`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Listing {
    /// Hidden, for this reason.
    Hidden(ExclusionReason),
    /// Listed as `pass`.
    Pass,
    /// Listed, not `pass`; `audience_open` when an audience check is the
    /// open one (an unanswered flag, an open list).
    Open { audience_open: bool },
}

impl Listing {
    pub(crate) fn of(decision: &Result<Decided, ExclusionReason>) -> Self {
        match decision {
            Err(reason) => Self::Hidden(*reason),
            Ok(d) if d.item.eligibility_status == "pass" => Self::Pass,
            Ok(d) => Self::Open {
                audience_open: d.audience_open,
            },
        }
    }
}

/// [`decide`] for one programme of the caller's snapshot, reading what the
/// list reads for it: audience text, youth_fit verdict, condition reviews.
/// `Ok(None)` when the profile lacks what the list needs (birth year,
/// region) or does not validate; else the [`Listing`].
pub(crate) async fn decide_on(
    connection: &mut sqlx::PgConnection,
    prog: &Program,
    university_id: Option<&str>,
    profile: Option<&mazimi_core::models::UserProfile>,
    school_id: Option<&str>,
    rule_row: Option<&EligibilityRule>,
    applicant: Option<ReviewEffect>,
) -> Result<Option<Listing>, sqlx::Error> {
    let Some(mut input) = profile
        .cloned()
        .and_then(super::benefit_assistant::bundles::profile_input)
    else {
        return Ok(None);
    };
    if super::profile_validation::normalize_profile(&mut input).is_err() {
        // Every card of this user reads `unknown` until the row is fixed:
        // say so once per card, without the profile (arch review M2).
        tracing::warn!(
            user = %user_tag(profile.map(|p| p.user_id)),
            "Stored profile fails validation; the card cannot read the list's decision"
        );
        return Ok(None);
    }
    let Some(gate) = sqlx::query_as::<_, GateColumns>(&format!(
        "SELECT {AUDIENCE_COLUMNS}, {YOUTH_FIT_COLUMNS} FROM current_programs WHERE id = $1"
    ))
    .bind(prog.id)
    .fetch_optional(&mut *connection)
    .await?
    else {
        return Ok(None);
    };
    let reviews = super::condition_reviews::load(&mut *connection, &[prog.id]).await?;
    let facts = gate.facts(prog, applicant);
    let decision = decide(
        prog,
        university_id,
        &input,
        school_id,
        rule_row,
        &facts,
        ReviewContext {
            facts: reviews.get(&prog.id),
            now: Utc::now(),
            section_enabled: verified_section_enabled(),
        },
    );
    Ok(Some(Listing::of(&decision)))
}

/// A user id as logs carry it: the first 12 hex letters of its SHA-256,
/// never the id itself.
fn user_tag(user: Option<Uuid>) -> String {
    use sha2::{Digest, Sha256};
    user.map_or_else(
        || "-".to_owned(),
        |u| {
            let digest = Sha256::digest(u.as_bytes());
            digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
        },
    )
}

/// The youth_fit gate facts of a profile (see [`youth_fit::GateProfile`]).
pub(crate) fn youth_fit_gate_profile(
    input: &ProfileInput,
    profile: &AudienceProfile,
) -> youth_fit::GateProfile {
    youth_fit::GateProfile {
        self_employed: profile.self_employed,
        has_disability: input.has_disability,
        veteran_family: input.veteran_family,
        age: profile.age,
        household_size: input.household_size,
    }
}

/// [`youth_fit_gate_profile`] of a preview input at the Seoul clock.
pub(crate) fn youth_fit_gate_for(input: &ProfileInput) -> youth_fit::GateProfile {
    let profile = audience_profile(input, mazimi_core::seoul_year(Utc::now()));
    youth_fit_gate_profile(input, &profile)
}

/// The stored youth_fit verdict and target of each programme in `ids` (a
/// reviewed pair wins), as the list reads them in `YOUTH_FIT_COLUMNS`.
pub(crate) async fn stored_tiers(
    connection: &mut sqlx::PgConnection,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, youth_fit::Tier>> {
    // Every programme, current or not (a saved row's card): `programs` under
    // the alias the shared columns correlate on.
    let rows: Vec<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(&format!(
        "SELECT current_programs.id, {YOUTH_FIT_COLUMNS} FROM programs current_programs \
         WHERE current_programs.id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, verdict, target)| {
            youth_fit::Tier::parse(verdict.as_deref()?, target.as_deref()).map(|t| (id, t))
        })
        .collect())
}

/// The membership of a row reserved for nobody (tests of rows whose
/// audience is not under test).
#[cfg(test)]
pub(crate) fn unreserved() -> audience::Membership {
    audience::membership(
        &audience::classify(&AudienceText::default()),
        None,
        &AudienceProfile::default(),
    )
}

/// The membership check of a loaded programme (`audience::membership` over
/// [`youth_fit::program_audience`]): the assistant card's reading of the
/// same text the list reads through `AUDIENCE_COLUMNS`.
pub(crate) fn program_membership(
    program: &Program,
    tier: Option<youth_fit::Tier>,
    profile: &AudienceProfile,
) -> audience::Membership {
    audience::membership(&youth_fit::program_audience(program), tier, profile)
}

/// The audience facts of a saved profile (none answered without one).
pub(crate) fn saved_audience_profile(
    profile: Option<&UserProfile>,
    current_year: i32,
) -> AudienceProfile {
    profile
        .cloned()
        .and_then(super::benefit_assistant::bundles::profile_input)
        .map(|input| audience_profile(&input, current_year))
        .unwrap_or_default()
}

pub(crate) fn audience_profile(input: &ProfileInput, current_year: i32) -> AudienceProfile {
    AudienceProfile {
        age: Some(current_year - input.birth_year),
        has_disability: input.has_disability,
        veteran_family: input.veteran_family,
        multicultural_family: input.is_multicultural_family,
        self_employed: input.employment_status.as_deref() == Some("self_employed"),
        holdings: input.holdings(),
    }
}

fn persona(input: &ProfileInput) -> Persona {
    let enrollment = input.enrollment_status.as_deref();
    let employment = input.employment_status.as_deref();
    Persona {
        student: matches!(
            enrollment,
            Some("enrolled" | "on_leave" | "research_student")
        ) || employment == Some("student"),
        job_seeker: matches!(employment, Some("unemployed" | "preparing")),
        renter: matches!(
            input.housing_type.as_deref().map(str::trim),
            Some("monthly_rent" | "jeonse" | "월세" | "전세")
        ),
    }
}

/// Ranking inputs the list knows about a row beyond the programme itself.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Relevance {
    /// The audience classifier found a youth target.
    pub youth: bool,
    /// The title itself names young people.
    pub youth_title: bool,
    /// The row serves the persona's category (student → scholarships, …).
    pub persona: bool,
}

impl Relevance {
    /// 0–10 relevance points: youth target (2, or 4 when the title names
    /// young people), persona category (4), district match (2).
    fn points(self, district: Option<DistrictMatch>) -> f64 {
        let youth = match (self.youth, self.youth_title) {
            (_, true) => 4.0,
            (true, false) => 2.0,
            (false, false) => 0.0,
        };
        let persona = if self.persona { 4.0 } else { 0.0 };
        let district = if district == Some(DistrictMatch::Matched) {
            2.0
        } else {
            0.0
        };
        youth + persona + district
    }
}

/// Rows limited to other sidos than the profile's (`$2`, its canonical
/// region; never true when `NULL`) with no `S-` code override that could
/// replace their age or region check. The structural gate hides each one:
/// its region check fails and no override removes it. A `regions` entry
/// `unknown` is an unresolved scope with its own reason, so it stays out.
const ELSEWHERE_SQL: &str = "$2::text IS NOT NULL \
    AND cardinality(regions) > 0 \
    AND NOT ('unknown' = ANY(regions)) \
    AND NOT ($2::text = ANY(regions)) \
    AND NOT EXISTS (SELECT 1 FROM program_condition_reviews r \
        WHERE r.source_type = current_programs.source_type \
          AND r.source_id = current_programs.source_id \
          AND r.code_overrides IS NOT NULL \
          AND jsonb_path_exists(r.code_overrides, '$[*] ? (@.target starts with \"S-\")'))";

/// Number of canonical sido keys; a programme listing all of them is
/// nationwide, not regional.
const SIDO_COUNT: usize = 17;

/// A sido-only programme for a user of that sido, recognised before
/// `region_districts` is collected: the only listed region is the user's and
/// the provider is the sido government, not one of its 시·군·구.
fn sido_government_programme(prog: &Program, input: &ProfileInput) -> bool {
    let Some(region) = mazimi_core::canonical_region_code(&input.region_code) else {
        return false;
    };
    prog.region_districts.is_none()
        && prog
            .regions
            .as_ref()
            .is_some_and(|regions| regions.len() == 1 && regions[0] == region)
        && prog
            .provider_name
            .as_deref()
            .is_some_and(|provider| structural::sido_wide_provider(provider, region))
}

/// Whether `regions` limits the programme to part of the country.
fn regional(prog: &Program) -> bool {
    prog.regions
        .as_ref()
        .is_some_and(|regions| !regions.is_empty() && regions.len() < SIDO_COUNT)
}

/// The `Program` columns a list decision and its ranking read. Display-only
/// text (summary, application guide, scopes, tags) is `NULL` here so a
/// request does not pull the text of the whole catalogue, and
/// `normalized_payload` (which repeats that text) is reduced to the keys
/// a decision reads: `enrollment_statuses` and `audience_scope` (the
/// structural check, [`structural::enrollment_statuses`]),
/// `display` (an auto-published corporate offer, [`auto_published_offer`])
/// and the income codes ([`mazimi_core::income::PAYLOAD_KEYS_SQL`]) with the
/// two text reductions the income check reads (`income_status_text`,
/// `income_bound_text`).
pub(crate) fn decision_columns() -> &'static str {
    static COLUMNS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    COLUMNS.get_or_init(|| {
        format!(
            "id, program_type, source_type, source_id, title, \
             NULL::text AS summary, provider_name, official_url, program_status, \
             application_start_at, application_end_at, benefit_amount_monthly, \
             benefit_amount_semester, benefit_amount_once, NULL::jsonb AS region_scope, \
             NULL::jsonb AS school_scope, NULL::jsonb AS tags, \
             jsonb_build_object('enrollment_statuses', normalized_payload->'enrollment_statuses', \
                                'audience_scope', normalized_payload->'audience_scope', \
                                'display', normalized_payload->'display', \
                                {income_keys}, \
                                'income_status_text', {status_text}, \
                                'income_bound_text', {bound_text}) \
             AS normalized_payload, min_age, \
             max_age, regions, deadline_at, benefit_valid_until, is_active, last_synced_at, \
             created_at, updated_at, company_name, company_logo_url, benefit_category, \
             provider_kind, NULL::jsonb AS application_steps, NULL::text AS application_method, \
             NULL::text AS submission_documents, NULL::text AS screening_method, region_districts, \
             eligibility_condition, holder_requirement, age_basis",
            income_keys = mazimi_core::income::PAYLOAD_KEYS_SQL,
            status_text = mazimi_core::income::status_text_sql(),
            bound_text = mazimi_core::income::bound_text_sql(),
        )
    })
}

/// `(id, application_method, submission_documents, screening_method,
/// summary)` of a listed row.
type GuideText = (
    Uuid,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(sqlx::FromRow)]
struct CatalogueRow {
    #[sqlx(flatten)]
    program: Program,
    university_id: Option<String>,
    currently_available: bool,
    #[sqlx(flatten)]
    gate: GateColumns,
    #[sqlx(flatten)]
    categories: mazimi_core::categories::CategoryRow,
}

/// The columns [`AUDIENCE_COLUMNS`] and [`YOUTH_FIT_COLUMNS`] read: the one
/// shape the list and [`decide_on`] decode, so a column added to either
/// constant is a compile-time field here, not a positional runtime error.
#[derive(sqlx::FromRow, Default)]
pub(crate) struct GateColumns {
    pub audience_target: Option<String>,
    pub applicant_kinds: Option<String>,
    pub target_tags: Option<String>,
    pub life_stages: Option<String>,
    pub audience_summary: Option<String>,
    pub curated_youth: bool,
    pub youth_fit: Option<String>,
    pub youth_fit_target: Option<String>,
}

impl GateColumns {
    /// The row facts of `prog` with these columns.
    pub(crate) fn facts<'a>(
        &'a self,
        prog: &'a Program,
        applicant: Option<ReviewEffect>,
    ) -> RowFacts<'a> {
        RowFacts {
            audience: AudienceText {
                source_type: &prog.source_type,
                title: &prog.title,
                summary: self.audience_summary.as_deref(),
                target: self.audience_target.as_deref(),
                applicant_kinds: self.applicant_kinds.as_deref(),
                target_tags: self.target_tags.as_deref(),
                life_stages: self.life_stages.as_deref(),
                min_age: prog.min_age,
                max_age: prog.max_age,
                curated_youth: self.curated_youth,
            },
            youth_fit: self.youth_fit.as_deref(),
            youth_fit_target: self.youth_fit_target.as_deref(),
            applicant,
            holder: prog
                .holder_requirement
                .as_deref()
                .and_then(mazimi_core::holder::Holder::parse),
            holder_unreadable: mazimi_core::holder::Holder::unreadable(
                prog.holder_requirement.as_deref(),
            ),
        }
    }
}

impl CatalogueRow {
    fn facts(&self, applicant: Option<ReviewEffect>) -> RowFacts<'_> {
        self.gate.facts(&self.program, applicant)
    }
}

/// A programme row with the campus scope column the shared model omits.
#[derive(sqlx::FromRow)]
pub(crate) struct ProgramRow {
    #[sqlx(flatten)]
    pub program: Program,
    pub university_id: Option<String>,
}

/// Structural facts of a preview input. `school_id` is the catalogue id the
/// entered school resolved to.
pub(crate) fn structural_profile<'a>(
    input: &'a ProfileInput,
    school_id: Option<&'a str>,
) -> StructuralProfile<'a> {
    StructuralProfile {
        birth_year: Some(input.birth_year),
        birth_date: input.birth_date,
        region_code: Some(&input.region_code),
        enrollment_status: input
            .enrollment_status
            .as_deref()
            .filter(|status| !status.is_empty()),
        university_id: school_id,
        city_code: input.city_code.as_deref(),
    }
}

/// Decide one programme for a recommendation list: `Err` = hidden, with the
/// reason the filter summary counts. Pure, so the "listed pass never
/// explained as fail" property is unit-testable.
#[cfg(test)]
pub(crate) fn evaluate(
    prog: &Program,
    university_id: Option<&str>,
    input: &ProfileInput,
    school_id: Option<&str>,
    rule_row: Option<&EligibilityRule>,
) -> Result<(f64, RecommendationItem), ExclusionReason> {
    let membership = program_membership(
        prog,
        None,
        &audience_profile(input, mazimi_core::seoul_year(Utc::now())),
    );
    if membership.hides() {
        return Err(ExclusionReason::TargetGroup);
    }
    evaluate_ranked(
        prog,
        university_id,
        input,
        school_id,
        rule_row,
        &membership,
        Relevance::default(),
        ReviewContext::none(Utc::now()),
    )
}

/// The reviews of one row and the clock the predicate reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReviewContext<'a> {
    pub facts: Option<&'a ReviewFacts>,
    pub now: DateTime<Utc>,
    /// [`verified_section_enabled`]: a verified row is shown as one (`pass`,
    /// nothing missing, listed first) only when on.
    pub section_enabled: bool,
}

impl ReviewContext<'_> {
    #[cfg(test)]
    pub(crate) fn none(now: DateTime<Utc>) -> Self {
        Self {
            facts: None,
            now,
            section_enabled: false,
        }
    }
}

/// [`evaluate`] with what the list knows about the row's relevance and
/// reviews.
// `membership` is a required argument on purpose: no caller can reach a
// status without the audience check (review R-H3).
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_ranked(
    prog: &Program,
    university_id: Option<&str>,
    input: &ProfileInput,
    school_id: Option<&str>,
    rule_row: Option<&EligibilityRule>,
    membership: &audience::Membership,
    relevance: Relevance,
    review: ReviewContext<'_>,
) -> Result<(f64, RecommendationItem), ExclusionReason> {
    evaluate_assumed(
        prog,
        university_id,
        input,
        school_id,
        rule_row,
        membership,
        relevance,
        review,
        &[],
    )
}

/// [`evaluate_ranked`] with `assumed` profile fields (`region_code`,
/// `enrollment_status`) the search read from the question, not from the
/// saved profile (arch review M3). A check they pass counts as unknown, at
/// check level, so the status, `verified`, the condition counts and the
/// reasons all follow from here; a check they fail still hides the row, as
/// the search runs under that reading.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_assumed(
    prog: &Program,
    university_id: Option<&str>,
    input: &ProfileInput,
    school_id: Option<&str>,
    rule_row: Option<&EligibilityRule>,
    membership: &audience::Membership,
    relevance: Relevance,
    review: ReviewContext<'_>,
    assumed: &[&str],
) -> Result<(f64, RecommendationItem), ExclusionReason> {
    let profile = structural_profile(input, school_id);
    // Rule leaves on `age` count it on the programme's reference days, as
    // the structural age check does ([`rule_engine::AgeBasis`]).
    let basis = rule_engine::AgeBasis::for_program(prog, mazimi_core::seoul_date(review.now));
    // Code overrides of a current review ([`overrides`]): an overridden
    // structural check is replaced by its replacement (or dropped), an
    // overridden compiled source condition is taken out of the rule row.
    let review_overrides: &[CodeOverride] = review
        .facts
        .and_then(|f| f.review.as_ref())
        .filter(|r| r.current)
        .map_or(&[], |r| r.code_overrides.as_slice());
    let (applied, admitting): (Vec<&CodeOverride>, Vec<&CodeOverride>) =
        if review_overrides.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let columns = CodeColumns::of(prog, rule_row.and_then(|r| r.hard_filter_json.as_ref()));
            let applied = overrides::applicable(review_overrides, &columns);
            // Read before the gate and the stripping below: the code rules
            // as they stand, for this profile.
            let admitting = overrides::admitting(
                prog,
                university_id,
                &profile,
                input,
                &applied,
                &columns,
                mazimi_core::seoul_date(review.now),
            );
            (applied, admitting)
        };
    let (checks, override_evals) = overrides::structural_gate(
        prog,
        university_id,
        &profile,
        input,
        &applied,
        mazimi_core::seoul_date(review.now),
    )?;
    let mut checks = assume_checks(checks, assumed);
    // Who the row is reserved for is one more check (audience::membership):
    // the status never exceeds it, and a known outsider is hidden.
    checks.extend(membership.checks().iter().cloned());
    if let Some(reason) = structural::exclusion(&checks) {
        return Err(reason);
    }
    let mut override_evals: Vec<RuleEvalResult> = override_evals
        .into_iter()
        .map(|e| assume_eval(e, assumed))
        .collect();
    let code_targets: Vec<&CodeOverride> = applied
        .iter()
        .copied()
        .filter(|o| o.structural_kind().is_none())
        .collect();
    // The legacy gate below is for rows written before hard filters
    // existed. A row whose hard filter an override emptied is not one: its
    // remaining soft conditions only rank, as before the override.
    let had_hard = rule_row.is_some_and(|row| row.hard_filter_json.is_some());
    let stripped;
    let rule_row = if code_targets.is_empty() {
        rule_row
    } else {
        for o in &code_targets {
            if let Some(replacement) = &o.replacement {
                let evaluation = assume_eval(
                    overrides::evaluate_replacement(input, replacement, &basis),
                    assumed,
                );
                if evaluation.verdict == RuleVerdict::Fail {
                    return Err(o.exclusion());
                }
                override_evals.push(evaluation);
            }
        }
        stripped = rule_row.and_then(|row| strip_rule(row, &code_targets));
        stripped.as_ref()
    };
    let override_checks: Vec<&str> = applied.iter().map(|o| o.check_kind.as_str()).collect();
    let override_exceptions: Vec<&str> = admitting.iter().map(|o| o.target.as_str()).collect();
    let structural_pass = structural::combined(&checks) == RuleVerdict::Pass;
    // `None` = the programme states no such condition: nothing to claim.
    // A bare birth year leaves the 만나이 ambiguous by one year; the boundary
    // cohort stays visible as unknown and is asked to confirm the birth date.
    let status_of = |kind| checks.iter().find(|c| c.kind == kind).map(|c| c.status);
    let shown = Shown {
        age: status_of(CheckKind::Age),
        region: status_of(CheckKind::Region),
    };
    let district = structural::district_check(prog, &profile).1;
    let signals = RankSignals {
        region_confirmed: district.is_some() || sido_government_programme(prog, input),
        relevance: relevance.points(district),
    };
    // A coded income condition against the user's 기준 중위소득 band
    // ([`mazimi_core::income`]): a band wholly above the bound hides the row.
    // A band within the bound on a notice that names a status (수급자,
    // 차상위, …) stays Unknown with its own named check.
    let income_check = mazimi_core::income::check(prog, input.income_median_band.as_deref());
    let income = income_check.map(|c| c.verdict);
    if income == Some(RuleVerdict::Fail) {
        return Err(ExclusionReason::Income);
    }
    let income_answered = input
        .income_median_band
        .as_deref()
        .is_some_and(|band| band != rule_engine::INCOME_MEDIAN_UNKNOWN);
    // (label, the profile field that would answer it).
    let income_open: Option<(String, Option<&str>)> = income_check
        .filter(|c| c.verdict == RuleVerdict::Unknown)
        .map(|c| {
            if c.status_unconfirmed {
                (mazimi_core::income::STATUS_CHECK_LABEL.to_owned(), None)
            } else if income_answered {
                (explain_missing(mazimi_core::income::PROFILE_FIELD), None)
            } else {
                (
                    explain_missing(mazimi_core::income::ANSWER_FIELD),
                    Some(mazimi_core::income::ANSWER_FIELD),
                )
            }
        });
    let income_missing: Option<String> = income_open.as_ref().map(|(label, _)| label.clone());
    // The predicate reads the same income check (audit M2): an open one
    // withholds "조건 확인됨" and is a remaining check.
    let list_checks: Vec<condition_review::ListCheck> = income_open
        .as_ref()
        .map(|(label, question)| condition_review::ListCheck {
            verdict: RuleVerdict::Unknown,
            kind: "income",
            label,
            question_id: *question,
        })
        .into_iter()
        .collect();
    let structural_missing: Vec<String> = checks
        .iter()
        .filter(|c| c.status != RuleVerdict::Pass)
        .filter_map(|c| match (c.missing_field, c.kind) {
            (Some(field), _) => Some(explain_missing(if field == "birth_year" {
                "age"
            } else {
                field
            })),
            // Reserved for a group the profile cannot confirm.
            (None, CheckKind::Audience) => Some(condition_review::labels::TARGET.to_owned()),
            (None, _) => None,
        })
        .chain(income_missing)
        .collect();

    // The hard filter is a publication contract: malformed rules must fail
    // closed and an explicit contradiction must never be ranked back into the
    // result by a high soft score.
    let hard_eval = match rule_row.and_then(|row| row.hard_filter_json.as_ref().map(|h| (row, h))) {
        Some((rule_row, hard_filter_json)) => {
            let hard_node = match rule_engine::parse_rule_json(hard_filter_json) {
                Ok(node) => node,
                Err(error) => {
                    tracing::error!(
                        program_id = %prog.id,
                        eligibility_rule_id = %rule_row.id,
                        error = %error,
                        "Invalid hard eligibility filter; excluding program"
                    );
                    return Err(ExclusionReason::RuleUnavailable);
                }
            };
            let evaluation = assume_eval(rule_engine::evaluate(input, &hard_node, &basis), assumed);
            if evaluation.verdict == RuleVerdict::Fail {
                return Err(ExclusionReason::HardRule);
            }
            Some(evaluation)
        }
        None => None,
    };

    // `Some(None)` = the row's rule_json cannot be parsed: ranking falls back
    // inline and the row cannot be verified.
    let eval_result =
        rule_row.map(
            |rule_row| match rule_engine::parse_rule_json(&rule_row.rule_json) {
                Ok(node) => Some(assume_eval(
                    rule_engine::evaluate(input, &node, &basis),
                    assumed,
                )),
                Err(e) => {
                    tracing::warn!(
                        program_id = %prog.id,
                        error = %e,
                        "Failed to parse rule_json; falling back to inline scoring"
                    );
                    None
                }
            },
        );

    // Rows created before `hard_filter_json` existed retain their legacy
    // behaviour until the data migration compiles a hard filter. A completely
    // contradicted legacy rule is still excluded; mixed soft signals remain
    // rankable.
    if let Some(Some(eval_result)) = &eval_result {
        let passes_legacy_gate = had_hard
            || hard_eval.is_some()
            || !eval_result.matched_rules.is_empty()
            || !eval_result.unknown_rules.is_empty()
            || eval_result.match_percentage > 0.0;
        if !passes_legacy_gate {
            return Err(ExclusionReason::HardRule);
        }
    }

    // Plan "verified conditions" §1.1: the reviewed rule and window join the
    // source rule and structural checks. Only reviewed facts of a current,
    // complete review hide a row.
    let source_rules: Vec<&RuleEvalResult> = hard_eval
        .iter()
        .chain(eval_result.iter().flatten())
        .chain(override_evals.iter())
        .collect();
    let counts = condition_counts(
        input,
        &basis,
        &checks,
        income,
        rule_row,
        &override_evals,
        assumed,
    );
    let facts = review.facts;
    let mut assessment = condition_review::assess(
        input,
        &ServeFacts {
            review: facts.and_then(|f| f.review.as_ref()),
            applicant: facts.and_then(|f| f.applicant),
            program_type: &prog.program_type,
            structural: &checks,
            source_rules: &source_rules,
            source_rule_error: matches!(eval_result, Some(None)),
            override_checks: &override_checks,
            override_exceptions: &override_exceptions,
            age_basis: &basis,
            list_checks: &list_checks,
            now: review.now,
        },
    );
    if let Some(reason) = assessment.hide {
        return Err(reason);
    }
    // A reviewed rule read under an assumed field verified nothing.
    assessment.verified &= assumed.is_empty();
    let reviewed = facts.is_some_and(|f| f.review.is_some());

    let Some(rule_row) = rule_row else {
        // ── Fallback: inline scoring (no rules in DB) ──
        let (score, reasons, mut missing) = inline_score(prog, input, shown, signals, review.now);
        merge_labels(&mut missing, structural_missing);
        let mut item = build_item(
            prog,
            score.round() as i32,
            reasons,
            missing,
            "unknown",
            false,
        );
        apply_assessment(&mut item, assessment, reviewed, review.section_enabled);
        item.conditions = counts;
        return Ok((score, item));
    };

    let Some(Some(eval_result)) = eval_result else {
        // The hard-filter decision (when present) still stands; only the
        // non-blocking ranking falls back inline.
        let (score, reasons, mut missing) = inline_score(prog, input, shown, signals, review.now);
        if let Some(hard_eval) = &hard_eval {
            merge_missing_checks(&mut missing, hard_eval);
        }
        merge_labels(&mut missing, structural_missing);
        let mut item = build_item(
            prog,
            score.round() as i32,
            reasons,
            missing,
            "unknown",
            false,
        );
        apply_assessment(&mut item, assessment, reviewed, review.section_enabled);
        item.conditions = counts;
        return Ok((score, item));
    };

    let final_score =
        rule_engine::compute_final_score_with(&eval_result, prog, signals, review.now);
    let mut missing_checks = structural_missing;
    merge_missing_checks(&mut missing_checks, &eval_result);
    if let Some(hard_eval) = &hard_eval {
        merge_missing_checks(&mut missing_checks, hard_eval);
    }
    // A rule of the source's own codes alone checked nothing a reviewer read
    // in the notice (review M2). A rule the profile cannot answer (any
    // unknown leaf, hard or soft, a structural check or an override's
    // replacement) verified nothing either: the row ranks by score with the
    // unchecked ones.
    let verified = !rule_engine::source_codes_only(
        &rule_row.rule_json,
        rule_row.hard_filter_json.as_ref(),
        rule_row.created_by.as_deref(),
    ) && eval_result.verdict == RuleVerdict::Pass
        && hard_eval
            .as_ref()
            .is_none_or(|e| e.verdict == RuleVerdict::Pass)
        && override_evals
            .iter()
            .all(|e| e.verdict == RuleVerdict::Pass)
        && structural_pass
        && income.is_none_or(|v| v == RuleVerdict::Pass)
        // A widened or dropped code rule leaves a check open: a row listed
        // through an override verified nothing, whatever rule is left.
        && applied.is_empty()
        // An auto-published corporate offer (shown as the official page's
        // own text) is never "조건 확인됨": its terms stay on that page.
        && !auto_published_offer(prog);
    if !verified {
        // A condition the profile contradicts was checked, not left open:
        // say so rather than "미확인".
        let mismatch = eval_result.verdict == RuleVerdict::Fail
            || override_evals
                .iter()
                .any(|e| e.verdict == RuleVerdict::Fail);
        missing_checks.insert(
            0,
            if mismatch {
                condition_review::labels::RULE_MISMATCH
            } else {
                UNVERIFIED_CONDITIONS_LABEL
            }
            .to_owned(),
        );
    }
    let status = if hard_eval
        .as_ref()
        .is_some_and(|e| e.verdict == RuleVerdict::Pass)
        && eval_result.verdict == RuleVerdict::Pass
        && structural_pass
        && income.is_none_or(|v| v == RuleVerdict::Pass)
        && prog.program_type != "corporate_benefit"
        // A widened or dropped code rule leaves a check open.
        && applied.is_empty()
    {
        "pass"
    } else {
        "unknown"
    };
    let mut item = build_item(
        prog,
        final_score.round() as i32,
        eval_result.explain_reasons,
        missing_checks,
        status,
        verified,
    );
    apply_assessment(&mut item, assessment, reviewed, review.section_enabled);
    item.conditions = counts;
    Ok((final_score, item))
}

/// [`ConditionCounts`] of one row: each structural check, the income check,
/// each top-level condition of the hard filter and of the ranking rule (an
/// `any` group is one condition with the group's verdict; one in both rules
/// counts once with its worse result), and each override replacement.
fn condition_counts(
    input: &impl rule_engine::EligibilityProfile,
    basis: &rule_engine::AgeBasis,
    checks: &[structural::StructuralCheck],
    income: Option<RuleVerdict>,
    rule_row: Option<&EligibilityRule>,
    replacements: &[RuleEvalResult],
    assumed: &[&str],
) -> ConditionCounts {
    fn rank(verdict: RuleVerdict) -> u8 {
        match verdict {
            RuleVerdict::Pass => 0,
            RuleVerdict::Unknown => 1,
            RuleVerdict::Fail => 2,
        }
    }
    let mut counts = ConditionCounts::default();
    for check in checks {
        counts.add(check.status);
    }
    if let Some(verdict) = income {
        counts.add(verdict);
    }
    let trees = rule_row.into_iter().flat_map(|row| {
        row.hard_filter_json
            .iter()
            .chain(std::iter::once(&row.rule_json))
    });
    let mut conditions: Vec<(Value, RuleVerdict)> = Vec::new();
    for condition in trees.flat_map(|tree| rule_engine::top_level_conditions(Some(tree))) {
        let Ok(node) = rule_engine::parse_rule_json(condition) else {
            continue;
        };
        let verdict = assume_eval(rule_engine::evaluate(input, &node, basis), assumed).verdict;
        // The same condition with another weight is the same condition.
        let mut key = condition.clone();
        if let Some(object) = key.as_object_mut() {
            object.remove("weight");
        }
        match conditions.iter_mut().find(|(k, _)| *k == key) {
            Some((_, seen)) if rank(verdict) > rank(*seen) => *seen = verdict,
            Some(_) => {}
            None => conditions.push((key, verdict)),
        }
    }
    for (_, verdict) in conditions {
        counts.add(verdict);
    }
    for replacement in replacements {
        counts.add(replacement.verdict);
    }
    counts
}

/// Whether a rule or check field is one of the `assumed` profile fields.
fn assumed_field(assumed: &[&str], field: &str) -> bool {
    assumed
        .iter()
        .any(|a| *a == field || (*a == "region_code" && matches!(field, "region" | "city_code")))
}

/// Structural checks with a pass on an assumed field read as unknown.
fn assume_checks(
    mut checks: Vec<structural::StructuralCheck>,
    assumed: &[&str],
) -> Vec<structural::StructuralCheck> {
    for check in &mut checks {
        let field = match check.kind {
            CheckKind::Region | CheckKind::District => "region_code",
            CheckKind::Enrollment => "enrollment_status",
            CheckKind::Age | CheckKind::School | CheckKind::Audience => continue,
        };
        if check.status == RuleVerdict::Pass && assumed_field(assumed, field) {
            check.status = RuleVerdict::Unknown;
        }
    }
    checks
}

/// A rule evaluation with its passes on assumed fields taken out: a tree
/// that passed through one is unknown, and their reasons are not stated.
fn assume_eval(mut e: RuleEvalResult, assumed: &[&str]) -> RuleEvalResult {
    if assumed.is_empty() {
        return e;
    }
    let before = e.matched_rules.len();
    e.matched_rules
        .retain(|d| !assumed_field(assumed, &d.field));
    if e.matched_rules.len() < before && e.verdict == RuleVerdict::Pass {
        e.verdict = RuleVerdict::Unknown;
    }
    e.explain_reasons
        .retain(|r| !assumed.iter().any(|f| mazimi_core::is_reason_for(f, r)));
    e
}

/// `row` without the source conditions the `C-` overrides target, in both
/// the hard filter and the ranking rule; `None` when nothing is left.
fn strip_rule(row: &EligibilityRule, targets: &[&CodeOverride]) -> Option<EligibilityRule> {
    let hard = overrides::strip_targets(row.hard_filter_json.as_ref(), targets);
    let soft = overrides::strip_targets(Some(&row.rule_json), targets);
    let rule_json = soft.or_else(|| hard.clone())?;
    let mut out = row.clone();
    out.rule_json = rule_json;
    out.hard_filter_json = hard;
    Some(out)
}

/// Put the predicate's result on a listed item. `window` is always set,
/// `verified` only while the section is enabled. While the section is enabled a verified row is also `pass`
/// with nothing missing: the conjunction it rests on passed every leaf (with
/// no hard filter, today's status rule alone would say `unknown`). With the
/// section off it keeps today's status and checks. A reviewed row that is not
/// verified lists its remaining checks first in `missing_checks`, for old
/// clients; an unreviewed row keeps its `missing_checks` as before. A row
/// with an [`overrides::EXCEPTION_KIND`] check has its `match_score` capped
/// at [`EXCEPTION_SCORE_CAP`].
fn apply_assessment(
    item: &mut RecommendationItem,
    assessment: Assessment,
    reviewed: bool,
    section_enabled: bool,
) {
    item.window = assessment.window;
    // The only eligibility claim a client may rest on: none while the
    // section is off (review 2026-10-21 M2).
    item.verified = section_enabled && assessment.verified;
    if assessment.verified {
        if section_enabled {
            item.eligibility_status = "pass".to_owned();
            item.conditions_verified = true;
            item.missing_checks.clear();
        }
    } else if reviewed {
        let mut merged: Vec<String> = Vec::new();
        for label in assessment
            .remaining_checks
            .iter()
            .map(|check| check.label.clone())
            .chain(item.missing_checks.drain(..))
        {
            if !merged.contains(&label) {
                merged.push(label);
            }
        }
        item.missing_checks = merged;
    }
    item.remaining_checks = assessment.remaining_checks;
    // Listed only on the notice's exception to a code rule: never a "보통"
    // or better match badge, whatever the ranking rule scored.
    if item
        .remaining_checks
        .iter()
        .any(|check| check.kind == overrides::EXCEPTION_KIND)
    {
        item.match_score = item.match_score.min(EXCEPTION_SCORE_CAP);
    }
}

/// Highest `match_score` of a row listed only through a code override: below
/// the clients' "보통" badge cutoff (50), so it shows as "낮음". Only the shown
/// score is capped; the ranking score is not (such rows already rank last).
const EXCEPTION_SCORE_CAP: i32 = 49;

/// Structural verdicts an explanation may cite. `None` = the programme has
/// no such condition, so no sentence may claim it was met.
#[derive(Debug, Clone, Copy)]
struct Shown {
    age: Option<RuleVerdict>,
    region: Option<RuleVerdict>,
}

fn merge_labels(target: &mut Vec<String>, labels: Vec<String>) {
    for label in labels {
        if !target.contains(&label) {
            target.push(label);
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `RecommendationItem` from a program and pre-computed fields.
fn build_item(
    prog: &Program,
    score: i32,
    reasons: Vec<String>,
    missing_checks: Vec<String>,
    eligibility_status: &str,
    conditions_verified: bool,
) -> RecommendationItem {
    let paid = mazimi_core::pays_support(&prog.program_type);
    RecommendationItem {
        conditions_verified,
        verified: false,
        remaining_checks: Vec::new(),
        window: None,
        eligibility_status: eligibility_status.to_owned(),
        program_id: prog.id,
        title: prog.title.clone(),
        title_display: mazimi_core::title_display::title_display(&prog.title),
        program_type: prog.program_type.clone(),
        summary_display: card_summary(prog.summary.as_deref()),
        provider_name: prog.provider_name.clone(),
        company_name: prog.company_name.clone(),
        match_score: score.min(100),
        // A corporate offer's amounts are no support money (review
        // 2026-10-21 H2): a client summing items must not add them.
        benefit_amount_monthly: prog.benefit_amount_monthly.filter(|_| paid),
        benefit_amount_semester: prog.benefit_amount_semester.filter(|_| paid),
        // Calendar date in Korea of the last second still open: an end at
        // 24:00 KST (00:00 of the next day) is that day's deadline, not the
        // next day's, and a KST date is never read as UTC.
        deadline: prog
            .deadline_date()
            .map(|d| d.format("%Y-%m-%d").to_string()),
        reasons,
        missing_checks,
        official_url: prog.official_url.clone(),
        application_method: prog.application_method.clone(),
        submission_documents: prog.submission_documents.clone(),
        screening_method: prog.screening_method.clone(),
        evidence: Vec::new(),
        categories: Default::default(),
        conditions: Default::default(),
    }
}

/// Longest card body sent per item: the card shows two lines, and the
/// recommendation list is not paged.
const CARD_SUMMARY_CHARS: usize = 160;

/// The programme summary cleaned for display and capped for the card.
fn card_summary(summary: Option<&str>) -> Option<String> {
    let text = mazimi_core::display_text::normalize(summary).display?;
    if text.chars().count() <= CARD_SUMMARY_CHARS {
        return Some(text);
    }
    let cut: String = text.chars().take(CARD_SUMMARY_CHARS - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// Merge missing fields from one evaluation without duplicating labels when a
/// condition appears in both the hard filter and the ranking rule.
fn merge_missing_checks(target: &mut Vec<String>, evaluation: &RuleEvalResult) {
    let mut labels: BTreeSet<String> = target.iter().cloned().collect();
    labels.extend(
        evaluation
            .unknown_rules
            .iter()
            .map(|detail| explain_missing(&detail.field)),
    );
    *target = labels.into_iter().collect();
}

/// Ranking for programs without an evaluable `eligibility_rules` row. Uses
/// the rule formula with no eligibility evidence; the list sorts these below
/// rule-checked rows ([`evidence_tier`]).
///
/// Returns `(score, reasons, missing_checks)`. Reasons only state what a
/// structural check actually passed: no sentence for a condition the
/// programme does not have, and never a target group. `missing_checks`
/// opens with [`UNVERIFIED_CONDITIONS_LABEL`]; `preview_on` later puts
/// [`INDIRECT_NOTE`] before it on a reviewed `indirect` row.
fn inline_score(
    prog: &Program,
    input: &ProfileInput,
    shown: Shown,
    signals: RankSignals,
    now: DateTime<Utc>,
) -> (f64, Vec<String>, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mut missing: Vec<String> = vec![UNVERIFIED_CONDITIONS_LABEL.to_owned()];

    // Only a regional programme is "for residents of" the user's sido; one
    // listing every sido is nationwide.
    if shown.region == Some(RuleVerdict::Pass) && regional(prog) {
        let region =
            mazimi_core::canonical_region_code(&input.region_code).unwrap_or(&input.region_code);
        reasons.push(format!(
            "{}{}에 맞아요.",
            mazimi_core::region_label(region),
            mazimi_core::RESIDENCE_REASON_STEM
        ));
    }

    match shown.age {
        Some(RuleVerdict::Pass) => reasons.push("현재 연령 조건에 맞아요.".to_string()),
        // Asked only while the date of birth is missing (review R-L1).
        Some(RuleVerdict::Unknown) if input.birth_date.is_none() => {
            missing.push(explain_missing("age"))
        }
        Some(RuleVerdict::Unknown) => {}
        Some(RuleVerdict::Fail) | None => {}
    }

    if let Some(deadline) = prog.effective_deadline() {
        let days_left = mazimi_core::days_until(deadline, now);
        if days_left <= 0 {
            reasons.push("오늘 마감이에요! 서두르세요.".to_string());
        } else if days_left <= 30 {
            reasons.push(format!("마감이 {days_left}일 남았어요! 서두르세요."));
        }
    }

    // No question here a row does not ask: a scholarship's income or
    // enrollment condition is the field its rule or structural check reads
    // (client audit R-H1), never a hard-coded 소득분위.

    let no_evidence = RuleEvalResult {
        verdict: RuleVerdict::Unknown,
        matched_rules: Vec::new(),
        failed_rules: Vec::new(),
        unknown_rules: Vec::new(),
        match_percentage: 0.0,
        explain_reasons: Vec::new(),
    };
    (
        rule_engine::compute_final_score_with(&no_evidence, prog, signals, now),
        reasons,
        missing,
    )
}

/// Human-readable Korean label for a missing profile field (used in the
/// rule-engine path where `unknown_rules` contains field names). Shares the
/// label table with the predicate's remaining checks.
fn explain_missing(field: &str) -> String {
    condition_review::profile_label(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`audience_gate`] with no stored tier, membership reduced to its
    /// number of checks.
    fn gate(
        text: &AudienceText,
        profile: &AudienceProfile,
        scope: RecommendScope,
    ) -> Result<(bool, usize), ExclusionReason> {
        audience_gate(text, None, false, None, profile, scope)
            .map(|(youth, m)| (youth, m.checks().len()))
    }
    use serde_json::json;

    const ALL_SIDOS: [&str; 17] = [
        "busan",
        "chungbuk",
        "chungnam",
        "daegu",
        "daejeon",
        "gangwon",
        "gwangju",
        "gyeongbuk",
        "gyeonggi",
        "gyeongnam",
        "incheon",
        "jeju",
        "jeonbuk",
        "jeonnam",
        "sejong",
        "seoul",
        "ulsan",
    ];

    fn program(
        title: &str,
        regions: Option<Vec<&str>>,
        ages: (Option<i32>, Option<i32>),
    ) -> Program {
        let now = Utc::now();
        serde_json::from_value(json!({
            "id": Uuid::new_v4(), "program_type": "benefit", "source_type": "gov_benefits",
            "source_id": null, "title": title, "summary": null, "provider_name": null,
            "official_url": null, "program_status": "active", "application_start_at": null,
            "application_end_at": null, "benefit_amount_monthly": null,
            "benefit_amount_semester": null, "benefit_amount_once": null, "region_scope": null,
            "school_scope": null, "tags": null, "raw_payload": null, "normalized_payload": null,
            "min_age": ages.0, "max_age": ages.1, "regions": regions, "deadline_at": null,
            "is_active": true, "last_synced_at": null, "created_at": now, "updated_at": now,
            "company_name": null, "company_logo_url": null, "benefit_category": null,
            "provider_kind": null, "application_steps": null, "application_method": null,
            "submission_documents": null, "screening_method": null
        }))
        .unwrap()
    }

    fn input(region: &str, city: Option<&str>) -> ProfileInput {
        serde_json::from_value(json!({
            "birth_year": mazimi_core::seoul_year(Utc::now()) - 24,
            "region_code": region, "city_code": city
        }))
        .unwrap()
    }

    fn passing_rule() -> EligibilityRule {
        let node = json!({"field": "region_code", "op": "eq", "value": "busan"});
        serde_json::from_value(json!({
            "id": Uuid::new_v4(), "program_id": Uuid::nil(), "rule_json": node,
            "hard_filter_json": node, "version": 1,
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap()
    }

    fn listed(p: &Program, i: &ProfileInput, rule: Option<&EligibilityRule>) -> RecommendationItem {
        evaluate(p, None, i, None, rule).expect("listed").1
    }

    /// The card body and subtitle (Figma 34065:2930) come with the item, so a
    /// list never builds a sentence from the match reasons in their place.
    #[test]
    fn item_carries_the_card_summary_and_provider() {
        let mut p = program("청년 월세", None, (None, None));
        p.summary = Some("  월세를\n지원합니다.  ".into());
        p.provider_name = Some("부산광역시".into());
        let item = build_item(&p, 10, vec![], vec![], "unknown", false);
        assert_eq!(item.provider_name.as_deref(), Some("부산광역시"));
        assert_eq!(item.company_name, None);
        let summary = item.summary_display.expect("summary");
        assert!(
            summary.starts_with("월세를") && !summary.ends_with(' '),
            "{summary}"
        );
        let mut long = program("긴 요약", None, (None, None));
        long.summary = Some("가".repeat(500));
        let capped = build_item(&long, 10, vec![], vec![], "unknown", false)
            .summary_display
            .expect("capped");
        assert_eq!(capped.chars().count(), CARD_SUMMARY_CHARS);
        assert!(capped.ends_with('…'));
        let empty = build_item(
            &program("빈 요약", None, (None, None)),
            10,
            vec![],
            vec![],
            "unknown",
            false,
        );
        assert_eq!(empty.summary_display, None);
        let wire = serde_json::to_value(&empty).unwrap();
        assert!(
            wire.get("summary_display").is_none() && wire.get("provider_name").is_none(),
            "absent fields are omitted"
        );
    }

    /// F6.5: structural checks, the income check and distinct top-level rule
    /// conditions are counted once each; total = met + unknown + failed.
    #[test]
    fn condition_counts_cover_every_evaluated_check_once() {
        let year = mazimi_core::seoul_year(Utc::now());
        // Age band (birth year in the boundary → unknown), region (pass),
        // income (unknown), and one rule leaf in both the hard filter and the
        // ranking rule (counted once, pass).
        let mut p = program("청년 월세", Some(vec!["busan"]), (Some(19), Some(34)));
        p.normalized_payload = Some(json!({"income_median_bands": ["0-50"]}));
        let mut i = input("busan", None);
        i.birth_year = year - 35;
        let item = listed(&p, &i, Some(&passing_rule()));
        let c = item.conditions;
        assert_eq!(
            (
                c.conditions_total,
                c.conditions_met,
                c.conditions_unknown,
                c.conditions_failed
            ),
            (4, 2, 2, 0)
        );
        // A soft (ranking) leaf the profile contradicts is a failed one.
        let hard = json!({"field": "region_code", "op": "eq", "value": "busan"});
        let soft = json!({"all": [hard.clone(),
                                  {"field": "enrollment_status", "op": "eq", "value": "enrolled"}]});
        let rule: EligibilityRule = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "program_id": Uuid::nil(), "rule_json": soft,
            "hard_filter_json": hard, "version": 1,
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap();
        let mut graduate = input("busan", None);
        graduate.enrollment_status = Some("graduated".into());
        let plain = program("부산 지원", Some(vec!["busan"]), (None, None));
        let c = listed(&plain, &graduate, Some(&rule)).conditions;
        assert_eq!(
            (
                c.conditions_total,
                c.conditions_met,
                c.conditions_unknown,
                c.conditions_failed
            ),
            (3, 2, 0, 1)
        );
        // No rule, no band: only the region check.
        let c = listed(&plain, &input("busan", None), None).conditions;
        assert_eq!((c.conditions_total, c.conditions_met), (1, 1));
        // Served flat next to the other item fields, with the categories.
        let mut served = listed(&plain, &input("busan", None), None);
        served.categories = mazimi_core::categories::Categories::new(
            Some("policy".into()),
            vec!["policy".into()],
            Some("living".into()),
        );
        let v = serde_json::to_value(&served).unwrap();
        assert_eq!(v["conditions_total"], 1);
        assert_eq!(v["conditions_unknown"], 0);
        assert_eq!(v["field_label"], "생활");
        assert_eq!(v["provider_categories"], json!(["policy"]));
        assert_eq!(v["corporate_category"], Value::Null, "off the 기업혜택 tab");
        served.categories = mazimi_core::categories::Categories {
            corporate_category: Some("food".into()),
            ..mazimi_core::categories::Categories::new(
                Some("corporate".into()),
                vec!["corporate".into()],
                Some("living".into()),
            )
        };
        let v = serde_json::to_value(&served).unwrap();
        assert_eq!(
            v["corporate_category"], "food",
            "the chip, flat with the categories"
        );
    }

    /// Review R-M2 (arch M2): the income check reads the same thing through
    /// the decision query's reduced payload as through the loaded row. Each
    /// text of the unit corpus is stored, loaded both ways, and checked.
    #[tokio::test]
    async fn the_income_check_agrees_through_both_row_shapes() {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("isolated test database required (TEST_DATABASE_URL)");
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let texts = [
            "기준 중위소득 50% 이하 청년",
            "관내 거주 청년 누구나",
            "중위소득 50% 초과 100% 이하 청년",
            "중위소득 50% 이하 가구는 전액, 중위소득 120% 이하는 일부 지원",
            "기준중위소득의 50 % 이내 가구",
            "중위소득(가구원수별) 50% 이하",
            "중위소득 50% 또는 중위소득 75% 이하",
            "수급자 및 차상위계층(기준 중위소득 50% 이하)",
            // Review R-L3 shapes the reader reads (and the code run 0-50
            // contains): 퍼센트, 100분의, a space, a parenthetical,
            // full-width digits, a range, an after-word.
            "기준중위소득 50퍼센트 이하인 가구",
            "중위소득의 100분의 50이하인 자",
            "기준 중위 소득 50% 이하",
            "기준중위소득(4인가구 기준 6,097,773원)의 50% 이하",
            "중위소득 ５０％ 이하",
            "기준중위소득 30%~50%이하인 아동",
            "중위소득 50% 이하는 우선 선발",
            "기준 중위소득 40% 이하 가구",
        ];
        let tag = format!("incomeagree{}", Uuid::new_v4().simple());
        for (n, text) in texts.iter().enumerate() {
            for (summary, raw) in [
                (None, json!({"지원대상": text})),
                (Some(*text), json!({})),
                (Some("중위소득"), json!({"지원대상": text})),
                // Verify gap 1: the phrase split by a field boundary is not
                // a stated bound (both readers read field by field).
                (
                    Some("소득 기준: 기준 중위소득"),
                    json!({"지원대상": "50% 이하 가구의 청년", "선정기준": text}),
                ),
            ] {
                sqlx::query(
                    "INSERT INTO programs (program_type, source_type, title, program_status, \
                     is_active, summary, raw_payload, normalized_payload) \
                     VALUES ($1, 'gov_benefits', $2, 'active', true, $3, $4, $5)",
                )
                .bind(&tag)
                .bind(format!("{n}"))
                .bind(summary)
                .bind(&raw)
                .bind(json!({"income_median_bands": ["0-50"]}))
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        // Rows without coded bands (the query reads no text there): a
        // described 온통청년 condition, annual bounds, an empty band list.
        for (n, payload) in [
            json!({"youth_conditions": {"income_condition_text": "중위소득 50% 이하"}}),
            json!({"youth_conditions": {"income_annual_max_manwon": 3000}}),
            json!({"income_median_bands": []}),
            json!({}),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO programs (program_type, source_type, title, program_status, \
                 is_active, summary, raw_payload, normalized_payload) \
                 VALUES ($1, 'gov_benefits', $2, 'active', true, $3, $4, $5)",
            )
            .bind(&tag)
            .bind(format!("uncoded{n}"))
            .bind("기초생활수급자 중위소득 50% 이하 가구")
            .bind(json!({"지원대상": "차상위계층 기준 중위소득 50% 이하"}))
            .bind(payload)
            .execute(&pool)
            .await
            .unwrap();
        }
        let reduced: Vec<Program> = sqlx::query_as(&format!(
            // The list reads the decision columns from the view (they
            // include its derived `holder_requirement`).
            "SELECT {}, NULL::jsonb AS raw_payload FROM current_programs WHERE program_type = $1 ORDER BY id",
            decision_columns()
        ))
        .bind(&tag)
        .fetch_all(&pool)
        .await
        .unwrap();
        let full: Vec<Program> =
            sqlx::query_as("SELECT * FROM programs WHERE program_type = $1 ORDER BY id")
                .bind(&tag)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(reduced.len(), texts.len() * 4 + 4);
        for (r, f) in reduced.iter().zip(&full) {
            for band in ["le50", "le60", "le100", "unknown"] {
                assert_eq!(
                    mazimi_core::income::check(r, Some(band)),
                    mazimi_core::income::check(f, Some(band)),
                    "{:?} / {:?} / {band}",
                    f.summary,
                    f.raw_payload
                );
            }
        }
        sqlx::query("DELETE FROM programs WHERE program_type = $1")
            .bind(&tag)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// F6.1: a coded 중위소득 bound against the user's band: within passes,
    /// wholly above hides, across it or unanswered stays a named check.
    #[test]
    fn a_coded_income_bound_narrows_the_list_by_the_users_band() {
        let mut p = program("청년 월세", Some(vec!["busan"]), (None, None));
        p.normalized_payload = Some(json!({"income_median_bands": ["0-50", "51-75"]}));
        // Audit M1: the coded bound decides only when the notice states it.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 75% 이하 청년"}));
        let rule = passing_rule();
        let with_band = |band: Option<&str>| {
            let mut i = input("busan", None);
            i.income_median_band = band.map(str::to_owned);
            evaluate(&p, None, &i, None, Some(&rule))
        };
        for band in [None, Some("unknown")] {
            let item = with_band(band).expect("listed").1;
            assert_eq!(item.eligibility_status, "unknown", "{band:?}");
            assert!(!item.conditions_verified);
            assert!(
                item.missing_checks
                    .contains(&"기준 중위소득 구간을 입력하면 확인할 수 있어요".to_owned()),
                "{band:?}: {:?}",
                item.missing_checks
            );
        }
        // (60, 100] straddles the 75 % bound.
        let item = with_band(Some("le100")).expect("listed").1;
        assert_eq!(item.eligibility_status, "unknown");
        assert!(item
            .missing_checks
            .contains(&"소득 기준 확인 필요".to_owned()));
        let item = with_band(Some("le60")).expect("listed").1;
        assert_eq!(item.eligibility_status, "pass");
        assert!(item.conditions_verified);
        // HIGH-A: the same band on a notice that names 수급자·차상위 stays
        // unknown with a named check. A band above the bound does not hide
        // it either: the status may be another route (review B4).
        let mut status_row = p.clone();
        status_row.summary = Some("기초생활수급자 및 차상위계층 생활 지원".into());
        let mut le50 = input("busan", None);
        le50.income_median_band = Some("le50".into());
        let item = evaluate(&status_row, None, &le50, None, Some(&rule))
            .expect("listed")
            .1;
        assert_eq!(item.eligibility_status, "unknown");
        assert!(!item.conditions_verified);
        assert!(
            item.missing_checks
                .contains(&"수급자·차상위 등 자격 확인 필요".to_owned()),
            "{:?}",
            item.missing_checks
        );
        assert_eq!(item.conditions.conditions_unknown, 1);
        let mut le120 = le50.clone();
        le120.income_median_band = Some("le120".into());
        let item = evaluate(&status_row, None, &le120, None, Some(&rule))
            .expect("listed")
            .1;
        assert_eq!(item.eligibility_status, "unknown");
        assert!(item
            .missing_checks
            .contains(&"수급자·차상위 등 자격 확인 필요".to_owned()));
        assert_eq!(
            with_band(Some("le120")).err(),
            Some(ExclusionReason::Income)
        );
        assert_eq!(
            with_band(Some("gt180")).err(),
            Some(ExclusionReason::Income)
        );
        p.normalized_payload = None;
        let item = listed(&p, &input("busan", None), Some(&rule));
        assert_eq!(item.eligibility_status, "pass");
    }

    /// Review LOW-1: an `any` group is one condition with the group's
    /// verdict; a failing alternative inside it is not a failed condition.
    #[test]
    fn an_any_group_counts_once_with_its_verdict() {
        let node = json!({"any": [{"field": "region_code", "op": "eq", "value": "seoul"},
                                  {"field": "region_code", "op": "eq", "value": "busan"}]});
        let rule: EligibilityRule = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "program_id": Uuid::nil(), "rule_json": node,
            "hard_filter_json": node, "version": 1,
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap();
        let plain = program("부산 지원", None, (None, None));
        let c = listed(&plain, &input("busan", None), Some(&rule)).conditions;
        assert_eq!(
            (
                c.conditions_total,
                c.conditions_met,
                c.conditions_unknown,
                c.conditions_failed
            ),
            (1, 1, 0, 0)
        );
    }

    /// Audit F5: 02f62504 was shown to P2 with "서울 거주 청년 대상이에요." and
    /// "현재 연령 조건에 맞아요." although it has no age band and targets 100세.
    #[test]
    fn no_age_band_claims_no_age_and_region_names_residents_only() {
        let p = program(
            "100세 어르신 장수축하물품",
            Some(vec!["seoul"]),
            (None, None),
        );
        // Its audience (어르신) is the membership check's business, which the
        // list gate hides for this 24-year-old; here only the reasons matter.
        let item = evaluate_ranked(
            &p,
            None,
            &input("seoul", None),
            None,
            None,
            &unreserved(),
            Relevance::default(),
            ReviewContext::none(Utc::now()),
        )
        .expect("listed")
        .1;
        assert_eq!(item.reasons, ["서울 거주 조건에 맞아요."]);
        assert!(item
            .reasons
            .iter()
            .all(|r| !r.contains("청년") && !r.contains("연령")));
        assert_eq!(item.missing_checks[0], UNVERIFIED_CONDITIONS_LABEL);
        assert!(!item.conditions_verified);
        assert_eq!(item.eligibility_status, "unknown");
    }

    #[test]
    fn age_sentence_only_for_a_passed_band_and_boundary_asks() {
        let banded = program("청년 지원", None, (Some(19), Some(34)));
        let item = listed(&banded, &input("busan", None), None);
        assert!(item
            .reasons
            .contains(&"현재 연령 조건에 맞아요.".to_owned()));

        let year = mazimi_core::seoul_year(Utc::now());
        let mut boundary = input("busan", None);
        boundary.birth_year = year - 35; // 만 34 or 35
        let item = listed(&banded, &boundary, None);
        assert!(!item.reasons.iter().any(|r| r.contains("연령")));
        assert!(item.missing_checks.iter().any(|m| m.contains("생년")));
    }

    #[test]
    fn nationwide_rows_listing_every_sido_claim_no_residence() {
        let p = program(
            "청년미래적금",
            Some(ALL_SIDOS.to_vec()),
            (Some(19), Some(34)),
        );
        let item = listed(&p, &input("busan", None), None);
        assert!(
            !item.reasons.iter().any(|r| r.contains("거주")),
            "{:?}",
            item.reasons
        );
    }

    #[test]
    fn rule_rows_are_verified_and_rank_above_unverified_rows() {
        let p = program("부산 청년 지원", Some(vec!["busan"]), (None, None));
        let i = input("busan", None);
        let rule = passing_rule();
        let verified = listed(&p, &i, Some(&rule));
        assert!(verified.conditions_verified);
        assert!(!verified
            .missing_checks
            .contains(&UNVERIFIED_CONDITIONS_LABEL.to_owned()));
        assert_eq!(verified.eligibility_status, "pass");
        let unverified = listed(&p, &i, None);
        let mut partial = verified.clone();
        partial.eligibility_status = "unknown".into();
        assert_eq!(
            [&verified, &partial, &unverified].map(evidence_tier),
            [3, 2, 0],
            "pass > rule evidence > shown on trust"
        );
    }

    /// An auto-published corporate offer stays in the 확인 필요 tier even
    /// when a reviewed rule passes (a curated corporate row with a reviewed
    /// rule keeps its tier: `unknown_rule_evaluation_is_not_conditions_verified`).
    #[test]
    fn an_auto_published_corporate_offer_is_never_conditions_verified() {
        let mut p = program("예시샵 학생 혜택", Some(vec!["busan"]), (None, None));
        p.program_type = "corporate_benefit".into();
        let curated = listed(&p, &input("busan", None), Some(&passing_rule()));
        assert!(curated.conditions_verified, "{curated:?}");
        p.normalized_payload = Some(json!({"display": "verbatim"}));
        let item = listed(&p, &input("busan", None), Some(&passing_rule()));
        assert!(!item.conditions_verified, "{item:?}");
        assert_ne!(item.eligibility_status, "pass");
        assert_eq!(evidence_tier(&item), 0);
        assert_eq!(item.missing_checks[0], UNVERIFIED_CONDITIONS_LABEL);
    }

    /// Review M2: a rule made only of the source's own codes
    /// (`ingestion:source_codes`) checked nothing the notice states, so the
    /// row is not "conditions verified". A reviewed condition next to them is.
    #[test]
    fn source_code_rules_alone_are_not_verified_conditions() {
        let p = program("부산 청년 지원", Some(vec!["busan"]), (None, None));
        let i = input("busan", None);
        let own = json!({"field": "region_code", "op": "eq", "value": "busan",
            "weight": 20, "origin": "source_codes"});
        let mut rule = passing_rule();
        rule.rule_json = json!({"all": [own]});
        rule.hard_filter_json = Some(json!({"all": [own]}));
        rule.created_by = Some("ingestion:source_codes".into());
        let item = listed(&p, &i, Some(&rule));
        assert!(!item.conditions_verified, "{item:?}");
        assert_eq!(item.missing_checks[0], UNVERIFIED_CONDITIONS_LABEL);

        let reviewed = json!({"field": "region_code", "op": "eq", "value": "busan"});
        rule.rule_json = json!({"all": [own, reviewed]});
        let item = listed(&p, &i, Some(&rule));
        assert!(item.conditions_verified, "{item:?}");
        assert!(!item
            .missing_checks
            .contains(&UNVERIFIED_CONDITIONS_LABEL.to_owned()));

        // Audit M1: a pass on the source's codes alone is not evidence the
        // notice rule passed; it ranks below a notice rule that passed
        // without a hard filter, and it is not counted as confirmed.
        rule.rule_json = json!({"all": [own]});
        let codes_only = listed(&p, &i, Some(&rule));
        assert_eq!(codes_only.eligibility_status, "pass");
        rule.rule_json = json!({"all": [reviewed]});
        rule.hard_filter_json = None;
        let notice = listed(&p, &i, Some(&rule));
        assert_eq!(notice.eligibility_status, "unknown");
        assert!(notice.conditions_verified, "{notice:?}");
        assert!(
            evidence_tier(&notice) > evidence_tier(&codes_only),
            "{} vs {}",
            evidence_tier(&notice),
            evidence_tier(&codes_only)
        );
    }

    /// Prod 5fa3af29 (Nike 대학생 할인, migration 20261018000015): a 재학
    /// profile without `school_type` left the education rule `unknown` yet
    /// the row was "conditions verified" and ranked above higher-scoring
    /// rows. Only a fully eligible rule evaluation earns the rule tier.
    #[test]
    fn unknown_rule_evaluation_is_not_conditions_verified() {
        let mut p = program("나이키 대학생 10% 할인", None, (None, None));
        p.program_type = "corporate_benefit".into();
        let stage = json!({"field": "education_stage", "op": "in",
            "value": ["univ_enrolled", "univ_expected", "postgrad"]});
        let mut rule = passing_rule();
        rule.rule_json = json!({"all": [{"field": "education_stage", "op": "in",
            "value": ["univ_enrolled", "univ_expected", "postgrad"], "weight": 100}]});
        rule.hard_filter_json = Some(json!({"all": [stage]}));
        rule.created_by = Some("migration:20261018000015".into());

        let mut no_school = input("busan", None);
        no_school.enrollment_status = Some("재학".into());
        let unknown = listed(&p, &no_school, Some(&rule));
        assert_eq!(unknown.eligibility_status, "unknown", "{unknown:?}");
        assert!(!unknown.conditions_verified, "{unknown:?}");
        assert_eq!(evidence_tier(&unknown), 0);
        assert!(unknown
            .remaining_checks
            .iter()
            .any(|c| c.kind == "conditions_unreviewed"));

        // Unknown rows keep plain score order against rows with no rule.
        let other = listed(
            &program("부산 청년 지원", None, (None, None)),
            &no_school,
            None,
        );
        let mut ranked = vec![(23.0, unknown), (26.0, other.clone())];
        rank_items(&mut ranked, &HashSet::new(), false);
        assert_eq!(ranked[0].1.program_id, other.program_id);

        let mut student = no_school.clone();
        student.school_type = Some("university".into());
        student.school_year = Some(2);
        let eligible = listed(&p, &student, Some(&rule));
        assert!(eligible.conditions_verified, "{eligible:?}");
        assert!(eligible.reasons.iter().any(|r| r.contains("학력")));
        assert!(evidence_tier(&eligible) >= 1);
    }

    /// A soft rule with no hard filter and one failed condition confirmed
    /// nothing: no rule tier.
    #[test]
    fn a_failed_soft_condition_is_not_conditions_verified() {
        let p = program("부산 청년 지원", Some(vec!["busan"]), (None, None));
        let mut i = input("busan", None);
        i.income_bracket = Some(9);
        let mut rule = passing_rule();
        rule.rule_json = json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "income_bracket", "op": "lte", "value": 5}]});
        rule.hard_filter_json = None;
        let item = listed(&p, &i, Some(&rule));
        assert!(!item.conditions_verified, "{item:?}");
        assert_eq!(evidence_tier(&item), 0);
        assert_eq!(
            item.missing_checks[0],
            condition_review::labels::RULE_MISMATCH
        );
    }

    /// A passing rule next to a structural check the profile cannot answer
    /// (a district rule without `city_code`) is not "conditions verified".
    #[test]
    fn an_unknown_structural_check_is_not_conditions_verified() {
        let mut p = program("청년 자격증 응시료", Some(vec!["busan"]), (None, None));
        p.region_districts = Some(vec!["26500".into()]);
        let rule = passing_rule();
        let item = listed(&p, &input("busan", None), Some(&rule));
        assert!(item
            .missing_checks
            .contains(&"시·군·구 확인 필요".to_owned()));
        assert!(!item.conditions_verified, "{item:?}");
        let home = listed(&p, &input("busan", Some("26500")), Some(&rule));
        assert!(home.conditions_verified, "{home:?}");
    }

    #[test]
    fn district_is_plumbed_from_city_code() {
        // 부산 수영구 only (audit 00638ac6); P1 lives in 금정구.
        let mut p = program("청년 자격증 응시료", Some(vec!["busan"]), (None, None));
        p.region_districts = Some(vec!["26500".into()]);
        assert_eq!(
            evaluate(&p, None, &input("busan", Some("26410")), None, None).err(),
            Some(ExclusionReason::Region)
        );
        let unknown = listed(&p, &input("busan", None), None);
        assert!(unknown
            .missing_checks
            .contains(&"시·군·구 확인 필요".to_owned()));
        let home = listed(&p, &input("busan", Some("26500")), None);
        assert!(!home
            .missing_checks
            .contains(&"시·군·구 확인 필요".to_owned()));
    }

    /// A district-limited programme stays listed for a user in its sido who
    /// has not given a 시·군·구; the district is asked for, never assumed.
    #[test]
    fn district_programme_without_a_city_code_is_listed_as_unknown() {
        // 복지로 지자체 창원시 row (local_welfare v5): the 시 and all its 구.
        let mut p = program("창원 청년 지원", Some(vec!["gyeongnam"]), (None, None));
        p.source_type = "local_welfare".into();
        p.region_districts = Some(
            ["48120", "48121", "48123", "48125", "48127", "48129"]
                .map(String::from)
                .to_vec(),
        );
        let item = listed(&p, &input("gyeongnam", None), None);
        assert_eq!(item.eligibility_status, "unknown");
        assert!(item
            .missing_checks
            .contains(&"시·군·구 확인 필요".to_owned()));

        // A passing hard rule alone no longer confirms eligibility.
        let mut ruled = program("청년 자격증 응시료", Some(vec!["busan"]), (None, None));
        let rule = passing_rule();
        assert_eq!(
            listed(&ruled, &input("busan", None), Some(&rule)).eligibility_status,
            "pass"
        );
        ruled.region_districts = Some(vec!["26500".into()]);
        let item = listed(&ruled, &input("busan", None), Some(&rule));
        assert_eq!(item.eligibility_status, "unknown");
        assert!(item
            .missing_checks
            .contains(&"시·군·구 확인 필요".to_owned()));
    }

    #[test]
    fn district_match_ranks_above_an_unconfirmed_sido_match() {
        let mut district = program("청년 지원", Some(vec!["busan"]), (None, None));
        district.region_districts = Some(vec!["26410".into()]);
        let sido = program("청년 지원", Some(vec!["busan"]), (None, None));
        let nationwide = program("청년 지원", None, (None, None));
        let i = input("busan", Some("26410"));
        let score = |p: &Program| evaluate(p, None, &i, None, None).unwrap().0;
        assert!(score(&district) > score(&sido));
        assert_eq!(
            score(&sido),
            score(&nationwide),
            "no +10 for a sido-level match"
        );

        // Audit 00638ac6: a 수영구 programme collapsed to `busan` gets no bonus;
        // the city government's own programme does.
        let mut leaked = program("청년 자격증 응시료", Some(vec!["busan"]), (None, None));
        leaked.provider_name = Some("부산광역시 수영구".into());
        let mut city = program("청년 자격증 응시료", Some(vec!["busan"]), (None, None));
        city.provider_name = Some("부산광역시".into());
        assert_eq!(score(&leaked), score(&nationwide));
        assert_eq!(score(&city), score(&nationwide) + 5.0);
    }

    #[test]
    fn relevance_orders_youth_and_persona_rows_first() {
        let i: ProfileInput = serde_json::from_value(json!({
            "birth_year": 2004, "region_code": "busan", "enrollment_status": "enrolled"
        }))
        .unwrap();
        let p = program("x", None, (None, None));
        let score = |r: Relevance| {
            evaluate_ranked(
                &p,
                None,
                &i,
                None,
                None,
                &crate::services::recommendations::unreserved(),
                r,
                ReviewContext::none(Utc::now()),
            )
            .unwrap()
            .0
        };
        let plain = Relevance::default();
        let youth = Relevance {
            youth: true,
            ..plain
        };
        let titled = Relevance {
            youth: true,
            youth_title: true,
            ..plain
        };
        let student = Relevance {
            persona: true,
            ..titled
        };
        assert!(score(youth) > score(plain));
        assert!(score(titled) > score(youth));
        assert!(score(student) > score(titled));
        assert!(persona(&i).student);
    }

    #[test]
    fn audience_gate_applies_hard_exclusions_in_every_scope() {
        let senior = AudienceText {
            source_type: "gov_benefits",
            title: "100세 어르신 장수축하물품",
            target: Some("○ 관내 100세 어르신"),
            applicant_kinds: Some("개인"),
            ..Default::default()
        };
        let general = AudienceText {
            source_type: "gov_benefits",
            title: "전국민 마음투자 지원",
            target: Some("○ 정서적 어려움이 있는 국민"),
            applicant_kinds: Some("개인"),
            ..Default::default()
        };
        let youth = AudienceText {
            source_type: "youth_center",
            title: "청년도전지원사업",
            ..Default::default()
        };
        let profile = audience_profile(&input("seoul", None), mazimi_core::seoul_year(Utc::now()));
        for scope in [RecommendScope::Youth, RecommendScope::All] {
            assert_eq!(
                gate(&senior, &profile, scope),
                Err(ExclusionReason::TargetGroup)
            );
            assert_eq!(gate(&youth, &profile, scope), Ok((true, 0)));
        }
        assert_eq!(
            gate(&general, &profile, RecommendScope::Youth),
            Err(ExclusionReason::OutsideYouthScope)
        );
        assert_eq!(
            gate(&general, &profile, RecommendScope::All),
            Ok((false, 0))
        );
    }

    /// Review 2026-10-21 H2: a corporate offer's amounts are no support money
    /// (`pays_support`), so a list item never carries them; clients that sum
    /// item amounts cannot add a discount to a support total.
    #[test]
    fn corporate_items_carry_no_amounts() {
        let mut p = program("예시샵 학생 혜택", None, (None, None));
        p.benefit_amount_monthly = Some(30_000);
        p.benefit_amount_semester = Some(100_000);
        let paid = listed(&p, &input("busan", None), None);
        assert_eq!(paid.benefit_amount_monthly, Some(30_000));
        assert_eq!(paid.benefit_amount_semester, Some(100_000));
        p.program_type = "corporate_benefit".into();
        for rule in [None, Some(passing_rule())] {
            let item = listed(&p, &input("busan", None), rule.as_ref());
            assert_eq!(item.benefit_amount_monthly, None, "{item:?}");
            assert_eq!(item.benefit_amount_semester, None, "{item:?}");
        }
    }

    /// Review 2026-10-21 (Low): the date-only `deadline` is the KST day of
    /// the last instant still inside the window. Ingestion ends a day at
    /// 23:59:59 KST; an end at 24:00 KST (00:00 of the next day) is the same
    /// last day, not the next one.
    #[test]
    fn the_date_only_deadline_is_the_last_open_kst_day() {
        let at = |rfc3339: &str| {
            DateTime::parse_from_rfc3339(rfc3339)
                .unwrap()
                .with_timezone(&Utc)
        };
        for (end, day) in [
            ("2026-10-31T23:59:59+09:00", "2026-10-31"),
            ("2026-11-01T00:00:00+09:00", "2026-10-31"),
            ("2026-11-01T00:00:01+09:00", "2026-11-01"),
            ("2026-10-31T09:00:00+09:00", "2026-10-31"),
        ] {
            let mut p = program("청년 지원", None, (None, None));
            p.deadline_at = Some(at(end));
            let item = build_item(&p, 0, Vec::new(), Vec::new(), "pass", false);
            assert_eq!(item.deadline.as_deref(), Some(day), "{end}");
        }
    }

    /// Review 2026-10-21 (Low): inline scoring reads the request's clock, not
    /// its own `Utc::now()`, so one list never mixes two instants.
    #[test]
    fn inline_scoring_reads_the_request_clock() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut p = program("청년 지원", None, (None, None));
        p.deadline_at = Some(now + chrono::Duration::days(10) + chrono::Duration::hours(1));
        let item = evaluate_ranked(
            &p,
            None,
            &input("busan", None),
            None,
            None,
            &crate::services::recommendations::unreserved(),
            Relevance::default(),
            ReviewContext::none(now),
        )
        .unwrap()
        .1;
        assert!(
            item.reasons
                .contains(&"마감이 10일 남았어요! 서두르세요.".to_owned()),
            "{:?}",
            item.reasons
        );
        // Audit L4: days are Korean calendar days. Tomorrow 18:00 KST seen
        // today 20:00 KST is one day away, not "today".
        let evening = DateTime::parse_from_rfc3339("2026-01-01T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        p.deadline_at = Some(
            DateTime::parse_from_rfc3339("2026-01-02T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let item = evaluate_ranked(
            &p,
            None,
            &input("busan", None),
            None,
            None,
            &crate::services::recommendations::unreserved(),
            Relevance::default(),
            ReviewContext::none(evening),
        )
        .unwrap()
        .1;
        assert!(
            item.reasons
                .contains(&"마감이 1일 남았어요! 서두르세요.".to_owned()),
            "{:?}",
            item.reasons
        );
    }

    /// Audit M3: an auto-published "대학생" corporate offer
    /// (`audience_scope: higher_education`, no explicit statuses) is for
    /// university students: a graduate is hidden, an unanswered status is
    /// asked for, and the decision query carries the key.
    #[test]
    fn a_higher_education_offer_without_statuses_is_for_students() {
        let mut p = program("대학생 할인", None, (None, None));
        p.program_type = "corporate_benefit".into();
        p.source_type = "corporate_official".into();
        p.normalized_payload =
            Some(json!({"display": "verbatim", "audience_scope": "higher_education"}));
        let mut graduate = input("busan", None);
        graduate.birth_year = mazimi_core::seoul_year(Utc::now()) - 30;
        graduate.enrollment_status = Some("graduated".into());
        graduate.employment_status = Some("employed".into());
        assert_eq!(
            evaluate(&p, None, &graduate, None, None).err(),
            Some(ExclusionReason::Enrollment)
        );
        let unanswered = input("busan", None);
        let item = evaluate(&p, None, &unanswered, None, None)
            .expect("listed")
            .1;
        assert_eq!(item.eligibility_status, "unknown");
        assert!(
            item.missing_checks
                .contains(&condition_review::profile_label("enrollment_status")),
            "{:?}",
            item.missing_checks
        );
        let mut student = unanswered.clone();
        student.enrollment_status = Some("enrolled".into());
        assert!(evaluate(&p, None, &student, None, None).is_ok());
        assert!(
            decision_columns().contains("'audience_scope', normalized_payload->'audience_scope'")
        );
    }

    /// Client audit R-H1: a scholarship's missing check names the field its
    /// rule reads, never a hard-coded 소득분위 the programme may not ask.
    #[test]
    fn a_scholarship_asks_the_income_field_its_rule_reads() {
        let mut p = program("국가장학금", None, (None, None));
        p.program_type = "scholarship".into();
        let i = input("busan", None);
        let income = condition_review::profile_label("income_bracket");
        let kosaf = condition_review::profile_label("kosaf_support_bracket");
        let unruled = listed(&p, &i, None);
        assert!(
            !unruled.missing_checks.contains(&income),
            "{:?}",
            unruled.missing_checks
        );
        let mut rule = passing_rule();
        let node = json!({"field": "kosaf_support_bracket", "op": "lte", "value": 8});
        rule.rule_json = json!({"all": [node]});
        rule.hard_filter_json = Some(json!({"all": [node]}));
        let ruled = listed(&p, &i, Some(&rule));
        assert!(
            ruled.missing_checks.contains(&kosaf),
            "{:?}",
            ruled.missing_checks
        );
        // The question names the field the rule reads.
        let asked: Vec<_> = ruled
            .remaining_checks
            .iter()
            .filter_map(|c| c.question_id.as_deref())
            .collect();
        assert!(asked.contains(&"kosaf_support_bracket"), "{asked:?}");
        assert!(!asked.contains(&"income_bracket"), "{asked:?}");
    }

    #[test]
    fn a_requested_demoted_row_is_listed_but_never_passes() {
        // Review-final M-2: a question that asks for a demoted target keeps
        // its rows listed; the stored tier still caps them, as it caps a row
        // an exempt profile keeps (child_only for a likely parent).
        let rule = passing_rule();
        let mut older = input("busan", None);
        older.birth_year = mazimi_core::seoul_year(Utc::now()) - 30;
        for (verdict, target, requested) in [
            (
                "off_target",
                Some("farm_fishery"),
                &[youth_fit::Target::FarmFishery][..],
            ),
            ("off_target", Some("elder"), &[youth_fit::Target::Elder][..]),
            (
                "off_target",
                Some("other"),
                &[youth_fit::Target::Organisation][..],
            ),
            ("child_only", Some("child"), &[youth_fit::Target::Child][..]),
            ("child_only", Some("child"), &[][..]),
        ] {
            let p = program("생활 지원", Some(vec!["busan"]), (None, None));
            let gate = GateColumns {
                youth_fit: Some(verdict.to_owned()),
                youth_fit_target: target.map(str::to_owned),
                ..GateColumns::default()
            };
            let facts = gate.facts(&p, None);
            let profile = audience_profile(&older, mazimi_core::seoul_year(Utc::now()));
            let requested = if target == Some("other") {
                &[][..]
            } else {
                requested
            };
            let Ok((youth, membership)) =
                row_gates(&facts, &older, &profile, RecommendScope::All, requested)
            else {
                // The organisation row is not requested by its own target
                // word here: hidden, as before.
                assert_eq!(target, Some("other"));
                continue;
            };
            let (_, item) = evaluate_ranked(
                &p,
                None,
                &older,
                None,
                Some(&rule),
                &membership,
                Relevance {
                    youth,
                    ..Relevance::default()
                },
                ReviewContext::none(Utc::now()),
            )
            .expect("listed");
            assert_eq!(item.eligibility_status, "unknown", "{verdict} {target:?}");
            assert!(
                item.missing_checks
                    .contains(&condition_review::labels::TARGET.to_owned()),
                "{verdict} {target:?}: {:?}",
                item.missing_checks
            );
        }
    }

    /// A coded group no profile answers (`target_group`, ingestion H1) and
    /// a stored tier that caps the row (`audience::membership`, review-final
    /// M-2) are one fact: who the row is for. The card names it once, with
    /// the target wording, and asks no profile question for it; a coded
    /// gender reads as the gender check kind, also without a question.
    /// Verify V-M1: the list never attaches a question to a field the
    /// profile answered ("대학 (전문대 포함)" on a 전문대-only row); the check
    /// keeps its label.
    #[test]
    fn the_list_asks_no_question_for_an_answered_field() {
        let mut rule = passing_rule();
        let node = json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "school_type", "op": "in", "value": ["college"]}
        ]});
        rule.rule_json = node.clone();
        rule.hard_filter_json = Some(node);
        let mut student = input("busan", None);
        student.school_type = Some("university".into());
        let p = program("전문대 장학금", Some(vec!["busan"]), (None, None));
        let (_, item) = evaluate(&p, None, &student, None, Some(&rule)).expect("listed");
        assert_eq!(item.eligibility_status, "unknown");
        let school: Vec<_> = item
            .remaining_checks
            .iter()
            .filter(|c| c.label.contains("학교 유형"))
            .collect();
        assert!(!school.is_empty(), "{:?}", item.remaining_checks);
        assert!(
            school.iter().all(|c| c.question_id.is_none()),
            "{:?}",
            item.remaining_checks
        );
    }

    #[test]
    fn a_coded_group_and_a_capping_tier_name_the_target_once() {
        let mut rule = passing_rule();
        let node = json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "target_group", "op": "in", "value": ["farmer"], "severity": "label"},
            {"field": "gender", "op": "eq", "value": "female", "severity": "label"}
        ]});
        rule.rule_json = node.clone();
        rule.hard_filter_json = Some(node);
        let mut older = input("busan", None);
        older.birth_year = mazimi_core::seoul_year(Utc::now()) - 30;
        let p = program("영농 정착 지원", Some(vec!["busan"]), (None, None));
        let gate = GateColumns {
            youth_fit: Some("off_target".to_owned()),
            youth_fit_target: Some("farm_fishery".to_owned()),
            ..GateColumns::default()
        };
        let facts = gate.facts(&p, None);
        let profile = audience_profile(&older, mazimi_core::seoul_year(Utc::now()));
        let (youth, membership) = row_gates(
            &facts,
            &older,
            &profile,
            RecommendScope::All,
            &[youth_fit::Target::FarmFishery],
        )
        .expect("requested");
        let (_, item) = evaluate_ranked(
            &p,
            None,
            &older,
            None,
            Some(&rule),
            &membership,
            Relevance {
                youth,
                ..Relevance::default()
            },
            ReviewContext::none(Utc::now()),
        )
        .expect("listed");
        assert_eq!(item.eligibility_status, "unknown");
        let target = condition_review::labels::TARGET.to_owned();
        let gender = condition_review::CHECK_KINDS
            .iter()
            .find(|(kind, _)| *kind == "gender")
            .map(|(_, label)| (*label).to_owned())
            .unwrap();
        for (name, list) in [
            ("missing_checks", item.missing_checks.clone()),
            (
                "remaining_checks",
                item.remaining_checks
                    .iter()
                    .map(|c| c.label.clone())
                    .collect(),
            ),
        ] {
            assert_eq!(
                list.iter().filter(|l| l.contains("지원 대상")).count(),
                1,
                "{name}: {list:?}"
            );
            assert!(list.contains(&target), "{name}: {list:?}");
            assert_eq!(
                list.iter().filter(|l| l.contains("성별")).count(),
                1,
                "{name}: {list:?}"
            );
            assert!(list.contains(&gender), "{name}: {list:?}");
        }
        for field in mazimi_core::rule_engine::NO_PROFILE_ANSWER {
            assert!(
                item.remaining_checks
                    .iter()
                    .all(|c| c.question_id.as_deref() != Some(*field)),
                "{field}: {:?}",
                item.remaining_checks
            );
        }
    }

    /// Review R-H2 (twin of audit M5): a row reserved for a group the
    /// user is not known to be in is listed only as unknown: never pass,
    /// never verified, with the question or "지원 대상" as remaining check.
    /// An answered "no" on a closed list hides it; an answered "yes" passes.
    #[test]
    fn an_unanswered_reserved_group_is_listed_but_never_passes() {
        let year = mazimi_core::seoul_year(Utc::now());
        let rule = passing_rule();
        let list = |title: &str, target: &str, input: &ProfileInput| {
            let mut p = program(title, Some(vec!["busan"]), (None, None));
            p.source_type = "gov_benefits".into();
            p.raw_payload = Some(json!({"지원대상": target, "사용자구분": "개인"}));
            evaluate(&p, None, input, None, Some(&rule))
        };
        let unanswered = input("busan", None);
        let item = list("장애인 보조기기 지원", "○ 등록 장애인", &unanswered)
            .expect("listed")
            .1;
        assert_eq!(item.eligibility_status, "unknown");
        assert!(!item.conditions_verified && !item.verified);
        assert!(
            item.missing_checks
                .contains(&condition_review::profile_label("has_disability")),
            "{:?}",
            item.missing_checks
        );
        assert!(
            item.conditions.conditions_unknown >= 1,
            "{:?}",
            item.conditions
        );
        // An open list read for an answered "no": listed, unknown, 지원 대상.
        let mut no = unanswered.clone();
        no.has_disability = Some(false);
        no.veteran_family = Some(false);
        no.is_multicultural_family = Some(false);
        for target in [
            "○ 장애인(지체, 뇌병변)",
            "○ 장애인 및 그 가족",
            "○ 국가유공자 및 유족",
            "○ 40대 이상 중장년 구직자 및 재직자",
        ] {
            let item = list("생활 지원", target, &no).expect("listed").1;
            assert_eq!(item.eligibility_status, "unknown", "{target}");
            assert!(
                item.missing_checks
                    .contains(&condition_review::labels::TARGET.to_owned()),
                "{target}: {:?}",
                item.missing_checks
            );
        }
        // A closed list answered "no": hidden. Answered "yes": pass.
        assert_eq!(
            list("장애인 보조기기 지원", "○ 등록 장애인", &no).err(),
            Some(ExclusionReason::TargetGroup)
        );
        let mut yes = unanswered.clone();
        yes.has_disability = Some(true);
        let item = list("장애인 보조기기 지원", "○ 등록 장애인", &yes)
            .expect("listed")
            .1;
        assert_eq!(item.eligibility_status, "pass");
        // The youth_fit tier reads the same flag through the same check.
        let p = program("보조기기 지원", Some(vec!["busan"]), (None, None));
        let m = program_membership(
            &p,
            Some(youth_fit::Tier {
                verdict: youth_fit::Verdict::DisabilityOnly,
                target: Some(youth_fit::Target::Disability),
            }),
            &audience_profile(&unanswered, year),
        );
        assert_eq!(m.checks()[0].missing_field, Some("has_disability"));
        let item = evaluate_ranked(
            &p,
            None,
            &unanswered,
            None,
            Some(&rule),
            &m,
            Relevance::default(),
            ReviewContext::none(Utc::now()),
        )
        .unwrap()
        .1;
        assert_eq!(item.eligibility_status, "unknown");
    }

    /// Review H4: the assistant's search and bundles (`RecommendScope::All`)
    /// must reach 산모·신생아 programmes; the youth list leaves them out as
    /// outside its scope, not as a target-group exclusion.
    #[test]
    fn unflagged_groups_are_only_outside_the_youth_scope() {
        let birth = AudienceText {
            source_type: "gov_benefits",
            title: "산모·신생아 건강관리 지원",
            target: Some("○ 출산가정"),
            applicant_kinds: Some("개인"),
            ..Default::default()
        };
        let profile = audience_profile(&input("seoul", None), mazimi_core::seoul_year(Utc::now()));
        // Listed with one Unknown membership check: no profile answer
        // confirms 출산가정, so the row never passes (review R-H2).
        assert_eq!(gate(&birth, &profile, RecommendScope::All), Ok((false, 1)));
        assert_eq!(
            gate(&birth, &profile, RecommendScope::Youth),
            Err(ExclusionReason::OutsideYouthScope)
        );
    }

    /// Pre-merge (mutation g): a stored profile that fails validation (an
    /// out-of-range income bracket) gives the card no list decision, even on
    /// a row whose rule passes for a valid profile.
    #[tokio::test]
    async fn an_invalid_stored_profile_never_passes_the_card() {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL: this test needs a database");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,
                 regions,deadline_at,application_start_at)
             VALUES($1,'policy','manual',$2,'청년 생활 지원','active',true,ARRAY['busan'],
                 now()+interval '20 days', now()-interval '1 day')",
        )
        .bind(id)
        .bind(format!("invalid-profile-{id}"))
        .execute(&pool)
        .await
        .unwrap();
        let busan = json!({"field": "region_code", "op": "eq", "value": "busan"});
        sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version) VALUES($1,$2,$2,1)")
            .bind(id)
            .bind(&busan)
            .execute(&pool)
            .await
            .unwrap();
        let profile: mazimi_core::models::UserProfile = serde_json::from_value(json!({
            "user_id": Uuid::nil(), "birth_year": 2000, "region_code": "busan", "city_code": null,
            "school_name": null, "school_year": null, "enrollment_status": null,
            "employment_status": null, "major_group": null, "income_bracket": 11,
            "kosaf_support_bracket": null, "housing_type": null, "household_size": null,
            "has_disability": null, "is_multicultural_family": null,
            "is_low_income_household": null, "veteran_family": null,
            "preferred_categories": [], "school_type": null, "age_band": null,
            "profile_version": 1, "updated_at": Utc::now(), "nickname": null,
            "profile_image_url": null
        }))
        .unwrap();
        let prog: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let rule: Option<EligibilityRule> =
            sqlx::query_as("SELECT * FROM eligibility_rules WHERE program_id=$1")
                .bind(id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let listed = decide_on(
            &mut connection,
            &prog,
            None,
            Some(&profile),
            None,
            rule.as_ref(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(listed, None, "an invalid profile decides nothing");
        let mut valid = profile.clone();
        valid.income_bracket = Some(3);
        let control = decide_on(
            &mut connection,
            &prog,
            None,
            Some(&valid),
            None,
            rule.as_ref(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            control,
            Some(Listing::Pass),
            "the same row passes a valid profile"
        );
        // The card reads `unknown` without a decision
        // (`eligibility::tests::an_open_listing_never_passes_the_card`).
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Round 2 (arch M2a): the card's decision and the list agree on every
    /// row, hidden or listed: one row per reason the list can hide for, and
    /// listed rows with and without a passing rule. Same membership, same
    /// `pass`.
    #[tokio::test]
    async fn the_card_decision_and_the_list_agree_on_every_row() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let busan = json!({"field": "region_code", "op": "eq", "value": "busan"});
        // (case, insert columns, hard filter)
        let cases: Vec<(&str, Value, Option<Value>)> = vec![
            ("listed_pass", json!({}), Some(busan.clone())),
            ("listed_unknown", json!({}), None),
            // Published, not yet open (a closed one is not in current_programs).
            ("not_open", json!({"opens_in_days": 5}), None),
            ("age", json!({"max_age": 18}), None),
            ("region", json!({"regions": ["seoul"]}), None),
            (
                "school",
                json!({"program_type": "campus_benefit", "university_id": "pusan"}),
                None,
            ),
            (
                "enrollment",
                json!({"normalized_payload": {"enrollment_statuses": ["on_leave"]}}),
                None,
            ),
            ("scope", json!({"regions": ["unknown"]}), None),
            (
                "hard_rule",
                json!({}),
                Some(json!({"field": "region_code", "op": "eq", "value": "seoul"})),
            ),
            ("rule_unavailable", json!({}), Some(json!({"bogus": true}))),
            (
                "not_individual",
                // A non-youth title: for a youth programme the sibling's
                // `audience` reads 보조금24 kinds as individual.
                json!({"title": "사회복지시설 운영비 지원", "source_type": "gov_benefits",
                "raw_payload": {"사용자구분": "법인/시설/단체", "지원대상": "법인"}}),
                None,
            ),
            // Audience membership outcomes (the sibling's `audience::membership`).
            (
                "closed_outsider",
                json!({"title": "어르신 교통비 지원"}),
                None,
            ),
            (
                "open_list",
                json!({"title": "가족 돌봄 지원", "source_type": "gov_benefits",
                "raw_payload": {"사용자구분": "개인", "지원대상": "○ 장애인 및 그 가족"}}),
                Some(busan.clone()),
            ),
            (
                "unanswered_flag",
                json!({"title": "장애인 보조기기 지원", "source_type": "gov_benefits",
                "raw_payload": {"사용자구분": "개인", "지원대상": "○ 등록 장애인"}}),
                Some(busan.clone()),
            ),
            (
                "income",
                json!({"normalized_payload": {"income_median_bands": ["0-50"]}}),
                None,
            ),
        ];
        // The outcome each row must get from both the list and the card.
        let expected = |case: &str| match case {
            "listed_pass" => "pass",
            "listed_unknown" | "income" => "open",
            "open_list" | "unanswered_flag" => "open_audience",
            _ => "hidden",
        };
        let mut ids = Vec::new();
        for (case, columns, hard) in &cases {
            let id = Uuid::new_v4();
            let get = |k: &str| columns.get(k).cloned().unwrap_or(Value::Null);
            sqlx::query(
                "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,
                     regions,deadline_at,max_age,university_id,normalized_payload,raw_payload,application_start_at)
                 VALUES($1,COALESCE($2,'policy'),COALESCE($3,'manual'),$4,COALESCE($5,'청년 생활 지원'),'active',true,
                     $6,
                     COALESCE($7::timestamptz, now()+interval '20 days'),$8,$9,$10,$11,
                     now() + make_interval(days => $12))",
            )
            .bind(id)
            .bind(get("program_type").as_str().map(str::to_owned))
            .bind(get("source_type").as_str().map(str::to_owned))
            .bind(format!("agree-{case}-{id}"))
            .bind(get("title").as_str().map(str::to_owned))
            .bind(
                get("regions")
                    .as_array()
                    .map(|r| r.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                    .unwrap_or_else(|| vec!["busan".to_owned()]),
            )
            .bind(get("deadline_at").as_str().map(str::to_owned))
            .bind(get("max_age").as_i64().map(|n| n as i32))
            .bind(get("university_id").as_str().map(str::to_owned))
            .bind(columns.get("normalized_payload").cloned())
            .bind(columns.get("raw_payload").cloned())
            .bind(get("opens_in_days").as_i64().map_or(-1, |d| d as i32))
            .execute(&pool)
            .await
            .unwrap();
            if let Some(hard) = hard {
                sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version) VALUES($1,$2,$2,1)")
                    .bind(id)
                    .bind(hard)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            ids.push((*case, id));
        }
        let profile: mazimi_core::models::UserProfile = serde_json::from_value(json!({
            "user_id": Uuid::nil(), "birth_year": 2000, "region_code": "busan", "city_code": null,
            "school_name": null, "school_year": null, "enrollment_status": "enrolled",
            "employment_status": null, "major_group": null, "income_bracket": null,
            "kosaf_support_bracket": null, "housing_type": null, "household_size": null,
            "has_disability": null, "is_multicultural_family": null,
            "is_low_income_household": null, "veteran_family": null,
            "preferred_categories": [], "school_type": null, "age_band": null,
            "income_median_band": "le60", "profile_version": 1,
            "updated_at": Utc::now(), "nickname": null, "profile_image_url": null
        }))
        .unwrap();
        let mut member = profile.clone();
        member.has_disability = Some(true);
        let preview_of = |p: &mazimi_core::models::UserProfile| {
            super::super::benefit_assistant::bundles::profile_input(p.clone()).unwrap()
        };
        let list = preview(&pool, preview_of(&profile), RecommendScope::All)
            .await
            .unwrap();
        let member_list = preview(&pool, preview_of(&member), RecommendScope::All)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let mut hidden = 0;
        for (case, id) in &ids {
            let prog: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
                .bind(id)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            let rule: Option<EligibilityRule> =
                sqlx::query_as("SELECT * FROM eligibility_rules WHERE program_id=$1")
                    .bind(id)
                    .fetch_optional(&mut *connection)
                    .await
                    .unwrap();
            let university: Option<String> =
                sqlx::query_scalar("SELECT university_id FROM programs WHERE id=$1")
                    .bind(id)
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap();
            let card = decide_on(
                &mut connection,
                &prog,
                university.as_deref(),
                Some(&profile),
                None,
                rule.as_ref(),
                None,
            )
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{case}: the profile decides"));
            let listed = list.items.iter().find(|item| item.program_id == *id);
            let list_outcome = match listed {
                None => "hidden",
                Some(item) if item.eligibility_status == "pass" => "pass",
                Some(item)
                    if item.missing_checks.iter().any(|c| {
                        c == condition_review::labels::TARGET
                            || c == &condition_review::profile_label("has_disability")
                    }) =>
                {
                    "open_audience"
                }
                Some(_) => "open",
            };
            let card_outcome = match card {
                Listing::Hidden(_) => "hidden",
                Listing::Pass => "pass",
                Listing::Open {
                    audience_open: true,
                } => "open_audience",
                Listing::Open {
                    audience_open: false,
                } => "open",
            };
            assert_eq!(card_outcome, expected(case), "{case}: card {card:?}");
            assert_eq!(list_outcome, expected(case), "{case}: list {listed:?}");
            hidden += usize::from(matches!(card, Listing::Hidden(_)));
            // A member of the reserved group passes on both.
            if *case == "unanswered_flag" {
                let card = decide_on(
                    &mut connection,
                    &prog,
                    university.as_deref(),
                    Some(&member),
                    None,
                    rule.as_ref(),
                    None,
                )
                .await
                .unwrap();
                assert_eq!(card, Some(Listing::Pass), "member card");
                let item = member_list.items.iter().find(|item| item.program_id == *id);
                assert_eq!(
                    item.map(|i| i.eligibility_status.as_str()),
                    Some("pass"),
                    "member list"
                );
            }
        }
        assert_eq!(hidden, 10, "{hidden} hidden");
        let all: Vec<Uuid> = ids.iter().map(|(_, id)| *id).collect();
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&all)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Round 2 (arch M3): a profile field the search read from the question
    /// is an input of the decision. A check it passes counts as unknown, so
    /// status, verified, counts and reasons all follow; a contradiction
    /// still hides the row.
    #[test]
    fn an_assumed_field_never_passes_a_check() {
        let mut p = program("부산 청년 지원", Some(vec!["busan"]), (None, None));
        p.normalized_payload = Some(json!({"enrollment_statuses": ["enrolled"]}));
        let mut i = input("busan", None);
        i.enrollment_status = Some("enrolled".into());
        let rule = passing_rule();
        let decide = |assumed: &[&str]| {
            evaluate_assumed(
                &p,
                None,
                &i,
                None,
                Some(&rule),
                &unreserved(),
                Relevance::default(),
                ReviewContext::none(Utc::now()),
                assumed,
            )
        };
        let own = decide(&[]).unwrap().1;
        assert_eq!(own.eligibility_status, "pass");
        for assumed in [
            &["region_code"][..],
            &["enrollment_status"],
            &["region_code", "enrollment_status"],
        ] {
            let item = decide(assumed).unwrap().1;
            assert_eq!(item.eligibility_status, "unknown", "{assumed:?}");
            assert!(!item.conditions_verified, "{assumed:?}");
            assert!(
                item.conditions.conditions_met < own.conditions.conditions_met,
                "{assumed:?}"
            );
            for field in assumed {
                assert!(
                    !item
                        .reasons
                        .iter()
                        .any(|r| mazimi_core::is_reason_for(field, r)),
                    "{field}: {:?}",
                    item.reasons
                );
            }
        }
        // Re-review 6 H2, now at the decision (it was the search's rewrite):
        // a rule-checked row and an inline row (no rule) both lose the
        // residence reason under an assumed region and keep it otherwise.
        let daegu = program("대구 청년 지원", Some(vec!["daegu"]), (None, None));
        let mut rule_row = passing_rule();
        rule_row.rule_json = json!({"field": "region_code", "op": "eq", "value": "daegu"});
        rule_row.hard_filter_json = Some(rule_row.rule_json.clone());
        let asker = input("daegu", None);
        for rule in [Some(&rule_row), None] {
            let reasons = |assumed: &[&str]| {
                evaluate_assumed(
                    &daegu,
                    None,
                    &asker,
                    None,
                    rule,
                    &unreserved(),
                    Relevance::default(),
                    ReviewContext::none(Utc::now()),
                    assumed,
                )
                .unwrap()
                .1
                .reasons
            };
            assert!(
                reasons(&[])
                    .iter()
                    .any(|r| mazimi_core::is_residence_reason(r)),
                "{rule:?}"
            );
            assert!(
                !reasons(&["region_code"]).iter().any(|r| r.contains("거주")),
                "{rule:?}"
            );
        }
        // A contradiction under the assumed reading still hides the row.
        let seoul = input("seoul", None);
        assert!(evaluate_assumed(
            &p,
            None,
            &seoul,
            None,
            Some(&rule),
            &unreserved(),
            Relevance::default(),
            ReviewContext::none(Utc::now()),
            &["region_code"]
        )
        .is_err());
    }
}
