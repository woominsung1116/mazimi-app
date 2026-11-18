//! Bounded scenario discovery over the SAME ranked hard-filtered catalogue.
//! A scenario changes grouping, never the user's saved eligibility profile.
use super::super::{
    application_preparation::Error, benefit_comparison::FINANCIAL_PRODUCT_SQL, recommendations,
};
use mazimi_core::models::{
    FilterSummary, ProfileInput, RecommendScope, RecommendationItem, UserProfile,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    ReturningStudent,
    IndependentLiving,
    JobSearch,
    DailySavings,
}
#[derive(Serialize)]
pub struct Group {
    pub title: &'static str,
    pub basis: &'static str,
    pub total_candidates: usize,
    pub items: Vec<RecommendationItem>,
}
#[derive(Serialize)]
pub struct Bundle {
    pub version: u8,
    pub scenario: Scenario,
    pub profile_version: Option<i32>,
    pub required_profile_fields: Vec<&'static str>,
    pub groups: Vec<Group>,
    /// The recommendation filter the groups are drawn from (counts only).
    /// `null` until the saved profile has the required fields.
    pub filter_summary: Option<FilterSummary>,
}
struct Category {
    title: &'static str,
    /// Published `programs.benefit_category` values (worker enum). Decide
    /// first; `welfare`, `other` and missing categories are too generic.
    categories: &'static [&'static str],
    types: &'static [&'static str],
    words: &'static [&'static str],
}
const STUDY: Category = Category {
    title: "학비·학교생활",
    categories: &["scholarship"],
    types: &["scholarship", "campus_benefit"],
    words: &["등록금", "학자금", "재학생", "복학"],
};
const HOME: Category = Category {
    title: "주거·생활 기반",
    categories: &[],
    types: &[],
    words: &["월세", "주거", "기숙사", "전세", "이사비"],
};
const WORK: Category = Category {
    title: "취업·역량 준비",
    categories: &["recruitment", "internship", "education"],
    types: &[],
    words: &["취업", "구직", "면접", "인턴", "직업훈련", "자격증"],
};
const DAILY: Category = Category {
    title: "일상 소비 혜택",
    categories: &[
        "discount",
        "student_discount",
        "membership",
        "subscription",
        "software",
        "telecom",
        "transport",
        "culture",
    ],
    types: &["corporate_benefit"],
    words: &["교통비", "통신비", "문화패스"],
};
const LIVING: Category = Category {
    title: "생활비·지원",
    categories: &[],
    types: &[],
    words: &["생활비", "식비", "급식", "공과금", "에너지"],
};
fn categories(scenario: Scenario) -> &'static [Category] {
    match scenario {
        Scenario::ReturningStudent => &[STUDY, HOME, DAILY],
        Scenario::IndependentLiving => &[HOME, LIVING, DAILY],
        Scenario::JobSearch => &[WORK, LIVING, DAILY],
        Scenario::DailySavings => &[DAILY, LIVING, HOME],
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Basis {
    PublishedCategory,
    TypeOrTitleKeyword,
}

/// Category-first placement: the first scenario group whose published
/// categories include the item's category wins; only items without a specific
/// category fall back to programme type / title keywords.
fn place(
    groups: &[Category],
    category: Option<&str>,
    item: &RecommendationItem,
) -> Option<(usize, Basis)> {
    if let Some(category) = category {
        if let Some(i) = groups.iter().position(|g| g.categories.contains(&category)) {
            return Some((i, Basis::PublishedCategory));
        }
        if groups
            .iter()
            .flat_map(|g| g.categories)
            .chain(OTHER_SPECIFIC)
            .any(|c| *c == category)
        {
            // A specific category that belongs to no group in this scenario
            // (e.g. scholarship in job search) is not re-placed by keyword.
            return None;
        }
    }
    groups
        .iter()
        .position(|g| {
            g.types.contains(&item.program_type.as_str())
                || g.words.iter().any(|word| item.title.contains(word))
        })
        .map(|i| (i, Basis::TypeOrTitleKeyword))
}

/// Specific categories of every scenario, so an out-of-scenario category is
/// recognised as specific even when no group of this scenario lists it.
const OTHER_SPECIFIC: &[&str] = &[
    "scholarship",
    "recruitment",
    "internship",
    "education",
    "discount",
    "student_discount",
    "membership",
    "subscription",
    "software",
    "telecom",
    "transport",
    "culture",
];

fn group_basis(bases: &[Basis]) -> &'static str {
    match (
        bases.contains(&Basis::PublishedCategory),
        bases.contains(&Basis::TypeOrTitleKeyword),
    ) {
        (true, false) => "published_category",
        (false, true) => "type_or_title_keyword",
        (true, true) => "published_category_and_title_keyword",
        (false, false) => "published_category_or_title_keyword",
    }
}
pub(crate) fn profile_input(p: UserProfile) -> Option<ProfileInput> {
    Some(ProfileInput {
        birth_year: p.birth_year?,
        birth_date: p.birth_date,
        region_code: p.region_code?,
        city_code: p.city_code,
        school_name: p.school_name,
        school_year: p.school_year,
        enrollment_status: p.enrollment_status,
        employment_status: p.employment_status,
        major_group: p.major_group,
        income_bracket: p.income_bracket,
        kosaf_support_bracket: p.kosaf_support_bracket,
        housing_type: p.housing_type,
        household_size: p.household_size,
        has_disability: p.has_disability,
        is_multicultural_family: p.is_multicultural_family,
        is_low_income_household: p.is_low_income_household,
        veteran_family: p.veteran_family,
        preferred_categories: Some(p.preferred_categories),
        school_type: p.school_type,
        age_band: p.age_band,
        homeless_household: p.homeless_household,
        marital_status: p.marital_status,
        newlywed: p.newlywed,
        children_count: p.children_count,
        income_median_band: p.income_median_band,
        telecom_carrier: p.telecom_carrier,
        card_issuers: p.card_issuers,
        pay_apps: p.pay_apps,
    })
}
pub async fn build(pool: &PgPool, user: Uuid, scenario: Scenario) -> Result<Bundle, Error> {
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let profile = sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id=$1")
        .bind(user)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let mut response = Bundle {
        version: 1,
        scenario,
        profile_version: profile.as_ref().map(|p| p.profile_version),
        required_profile_fields: vec![],
        groups: vec![],
        filter_summary: None,
    };
    if profile.as_ref().and_then(|p| p.birth_year).is_none() {
        response.required_profile_fields.push("birth_year");
    }
    if profile
        .as_ref()
        .and_then(|p| p.region_code.as_deref())
        .is_none_or(|r| mazimi_core::canonical_region_code(r).is_none())
    {
        response.required_profile_fields.push("region_code");
    }
    if !response.required_profile_fields.is_empty() {
        tx.commit().await.map_err(crate::errors::internal_error)?;
        return Ok(response);
    }
    let input = profile
        .and_then(profile_input)
        .ok_or_else(|| crate::errors::internal_error("Validated profile missing fields"))?;
    let recommendation = recommendations::preview_on(&mut tx, input, RecommendScope::All).await?;
    response.filter_summary = Some(recommendation.filter_summary.clone());
    let ids: Vec<Uuid> = recommendation.items.iter().map(|i| i.program_id).collect();
    // Category and financial flag in one batch query, same snapshot.
    let meta: HashMap<Uuid, (Option<String>, bool)> =
        sqlx::query_as::<_, (Uuid, Option<String>, bool)>(&format!(
            "SELECT id, benefit_category, {FINANCIAL_PRODUCT_SQL} FROM programs WHERE id = ANY($1)"
        ))
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?
        .into_iter()
        .map(|(id, category, financial)| (id, (category, financial)))
        .collect();
    tx.commit().await.map_err(crate::errors::internal_error)?;
    let groups = categories(scenario);
    let mut placed: Vec<(Vec<RecommendationItem>, Vec<Basis>)> =
        groups.iter().map(|_| (vec![], vec![])).collect();
    for item in recommendation.items {
        let (category, financial) = meta
            .get(&item.program_id)
            .map(|(c, f)| (c.as_deref(), *f))
            .unwrap_or((None, false));
        if financial {
            continue;
        }
        if let Some((i, basis)) = place(groups, category, &item) {
            placed[i].0.push(item);
            placed[i].1.push(basis);
        }
    }
    for (category, (mut selected, mut bases)) in groups.iter().zip(placed) {
        let total_candidates = selected.len();
        selected.truncate(3);
        bases.truncate(3);
        response.groups.push(Group {
            title: category.title,
            basis: group_basis(&bases),
            total_candidates,
            items: selected,
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
    use serde_json::json;

    /// The group flags are three-state at rest (arch M1): an unanswered
    /// flag (`null`) does not Fail an `is_true` rule (온통청년 sbizCd 0014005,
    /// 보조금24 장애인/다문화); an answered "no" does.
    #[test]
    fn saved_flags_are_read_as_answered() {
        let saved: UserProfile = serde_json::from_value(json!({
            "user_id": uuid::Uuid::new_v4(), "birth_year": 2000, "region_code": "seoul",
            "has_disability": null, "is_multicultural_family": false,
            "is_low_income_household": null, "veteran_family": true,
            "preferred_categories": [], "profile_version": 1,
            "updated_at": "2026-10-15T00:00:00Z"
        }))
        .unwrap();
        let input = profile_input(saved).unwrap();
        assert_eq!(input.has_disability, None);
        assert_eq!(input.is_multicultural_family, Some(false));
        assert_eq!(input.is_low_income_household, None);
        assert_eq!(input.veteran_family, Some(true));
        let basis = mazimi_core::rule_engine::AgeBasis::today();
        let rule =
            |field: &str| parse_rule_json(&json!({"field": field, "op": "is_true"})).unwrap();
        assert_eq!(
            evaluate(&input, &rule("has_disability"), &basis).verdict,
            RuleVerdict::Unknown
        );
        assert_eq!(
            evaluate(&input, &rule("is_multicultural_family"), &basis).verdict,
            RuleVerdict::Fail
        );
    }
}
