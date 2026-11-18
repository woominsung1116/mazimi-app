use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use mazimi_core::models::{ProfileInput, UserProfile};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;

/// The base profile: the answers every client has sent since onboarding
/// existed. A save writes each as its body gives it (absent = cleared).
/// Frozen: a field added to `ProfileInput` never joins this list. Read by
/// the route tests, which hold every field to one of the two lists.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const BASE_PROFILE: [&str; 14] = [
    "birth_year",
    "region_code",
    "city_code",
    "school_name",
    "school_year",
    "enrollment_status",
    "employment_status",
    "major_group",
    "income_bracket",
    "housing_type",
    "household_size",
    "preferred_categories",
    "school_type",
    "age_band",
];

/// Every other answer is kept when a save's body omits its key: an
/// installed build that does not know a field never erases it. An explicit
/// `null` clears one. `newlywed` is written with `marital_status`;
/// `birth_date` has its own rule (below). The group flags are three-state
/// (`null` = not answered, `false` = "no").
pub(crate) const KEEP_WHEN_ABSENT: [&str; 12] = [
    "homeless_household",
    "marital_status",
    "children_count",
    "income_median_band",
    // Holder answers (mazimi_core::holder): no installed build sends them.
    "telecom_carrier",
    "card_issuers",
    "pay_apps",
    "kosaf_support_bracket",
    "has_disability",
    "is_multicultural_family",
    "is_low_income_household",
    "veteran_family",
];

/// POST /api/v1/profile
///
/// Upserts the authenticated user's profile.
/// The user's ID is taken from the JWT — the client must be authenticated.
///
/// Contract: the [`BASE_PROFILE`] answers are written as sent (an absent
/// one is cleared); every other answer is [`KEEP_WHEN_ABSENT`]: a body that
/// omits its key keeps the stored value, an explicit `null` clears it. Any
/// field added to `ProfileInput` from now on is keep-when-absent; the route
/// tests enumerate the fields, so one left out of both lists fails them.
///
/// `birth_date` is written only when the body names it (an explicit `null`
/// clears it). A body without it (an older client) keeps the stored date
/// while its year still equals the saved `birth_year`, and drops it
/// otherwise, so the two never disagree.
///
/// The first save that sets `birth_year` completes onboarding. A client that
/// asks for a nickname (the body has a `nickname` key, even `null`) must then
/// give one or have one stored (for example from Kakao). A client without a
/// nickname input (web today) sends no key and is not blocked.
pub async fn save_profile(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use crate::services::profile_validation::field_error;
    let user_id = auth_user.id;
    let birth_date_sent = body.get("birth_date").is_some();
    let nickname_sent = body.get("nickname").is_some();
    if body
        .get("birth_date")
        .is_some_and(|v| !v.is_null() && v.as_str().and_then(parse_date).is_none())
    {
        return Err(field_error(
            "생년월일을 다시 확인해 주세요.",
            "birth_date",
            "birth_date_invalid",
        ));
    }
    let present: Vec<bool> = KEEP_WHEN_ABSENT
        .iter()
        .map(|key| body.get(*key).is_some())
        .collect();
    let mut payload: SaveProfileRequest = serde_json::from_value(body).map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "프로필 형식이 올바르지 않아요."})),
        )
    })?;
    crate::services::profile_validation::check_city_code(&payload.profile)?;
    crate::services::profile_validation::normalize_profile(&mut payload.profile)?;
    let input = &payload.profile;
    let nickname = payload.nickname.as_deref().map(str::trim);
    if nickname.is_some_and(|name| {
        name.is_empty() || name.chars().count() > 30 || name.chars().any(char::is_control)
    }) {
        return Err(field_error(
            "닉네임은 1~30자로 입력해 주세요.",
            "nickname",
            "nickname_invalid",
        ));
    }
    // Onboarding is complete once a birth year is stored; sign-in creates
    // the row without one.
    let stored = sqlx::query_as::<_, (Option<i32>, Option<String>)>(
        "SELECT birth_year, nickname FROM user_profiles WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let onboarded = stored.as_ref().is_some_and(|(year, _)| year.is_some());
    let stored_nickname = stored
        .as_ref()
        .and_then(|(_, name)| name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty());
    if nickname_sent && !onboarded && nickname.is_none() && stored_nickname.is_none() {
        return Err(field_error(
            "닉네임을 입력해 주세요.",
            "nickname",
            "nickname_required",
        ));
    }

    // Upsert into user_profiles
    sqlx::query(
        r#"
        INSERT INTO user_profiles (
            user_id, birth_year, region_code, city_code, school_name, school_year,
            enrollment_status, employment_status, major_group, income_bracket,
            kosaf_support_bracket, housing_type, household_size,
            has_disability_answer, is_multicultural_family_answer,
            is_low_income_household_answer, veteran_family_answer,
            has_disability, is_multicultural_family, is_low_income_household,
            veteran_family, preferred_categories, school_type, age_band,
            nickname, homeless_household, marital_status, newlywed, children_count,
            income_median_band, birth_date, telecom_carrier, card_issuers, pay_apps, updated_at
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,
                  COALESCE($14, false), COALESCE($15, false), COALESCE($16, false),
                  COALESCE($17, false), $18,$19,$20,$21,
                  $22,$23,$24,$25,$26,$27,$33,$34,$35, now())
        ON CONFLICT (user_id) DO UPDATE SET
            birth_year = EXCLUDED.birth_year,
            birth_date = CASE
                WHEN $28 THEN EXCLUDED.birth_date
                WHEN EXTRACT(YEAR FROM user_profiles.birth_date)::int = EXCLUDED.birth_year
                    THEN user_profiles.birth_date
                ELSE NULL
            END,
            region_code = EXCLUDED.region_code,
            city_code = EXCLUDED.city_code,
            school_name = EXCLUDED.school_name,
            school_year = EXCLUDED.school_year,
            enrollment_status = EXCLUDED.enrollment_status,
            employment_status = EXCLUDED.employment_status,
            major_group = EXCLUDED.major_group,
            income_bracket = EXCLUDED.income_bracket,
            kosaf_support_bracket = CASE WHEN $39 THEN EXCLUDED.kosaf_support_bracket
                                         ELSE user_profiles.kosaf_support_bracket END,
            housing_type = EXCLUDED.housing_type,
            household_size = EXCLUDED.household_size,
            -- The answer columns are the three-state flags; the old bool
            -- columns follow them for an older image (migration
            -- 20261112000002, expand/contract).
            has_disability_answer = CASE WHEN $40 THEN EXCLUDED.has_disability_answer
                                         ELSE user_profiles.has_disability_answer END,
            is_multicultural_family_answer = CASE WHEN $41 THEN EXCLUDED.is_multicultural_family_answer
                                                  ELSE user_profiles.is_multicultural_family_answer END,
            is_low_income_household_answer = CASE WHEN $42 THEN EXCLUDED.is_low_income_household_answer
                                                  ELSE user_profiles.is_low_income_household_answer END,
            veteran_family_answer = CASE WHEN $43 THEN EXCLUDED.veteran_family_answer
                                         ELSE user_profiles.veteran_family_answer END,
            has_disability = CASE WHEN $40 THEN EXCLUDED.has_disability
                                  ELSE user_profiles.has_disability END,
            is_multicultural_family = CASE WHEN $41 THEN EXCLUDED.is_multicultural_family
                                           ELSE user_profiles.is_multicultural_family END,
            is_low_income_household = CASE WHEN $42 THEN EXCLUDED.is_low_income_household
                                           ELSE user_profiles.is_low_income_household END,
            veteran_family = CASE WHEN $43 THEN EXCLUDED.veteran_family
                                  ELSE user_profiles.veteran_family END,
            preferred_categories = EXCLUDED.preferred_categories,
            school_type = EXCLUDED.school_type,
            age_band = EXCLUDED.age_band,
            nickname = COALESCE(EXCLUDED.nickname, user_profiles.nickname),
            homeless_household = CASE WHEN $29 THEN EXCLUDED.homeless_household
                                      ELSE user_profiles.homeless_household END,
            marital_status = CASE WHEN $30 THEN EXCLUDED.marital_status
                                  ELSE user_profiles.marital_status END,
            newlywed = CASE WHEN $30 THEN EXCLUDED.newlywed ELSE user_profiles.newlywed END,
            children_count = CASE WHEN $31 THEN EXCLUDED.children_count
                                  ELSE user_profiles.children_count END,
            income_median_band = CASE WHEN $32 THEN EXCLUDED.income_median_band
                                      ELSE user_profiles.income_median_band END,
            telecom_carrier = CASE WHEN $36 THEN EXCLUDED.telecom_carrier
                                   ELSE user_profiles.telecom_carrier END,
            card_issuers = CASE WHEN $37 THEN EXCLUDED.card_issuers
                                ELSE user_profiles.card_issuers END,
            pay_apps = CASE WHEN $38 THEN EXCLUDED.pay_apps ELSE user_profiles.pay_apps END,
            profile_version = user_profiles.profile_version + 1,
            updated_at = now()
        "#,
    )
    .bind(user_id)
    .bind(input.birth_year)
    .bind(&input.region_code)
    .bind(&input.city_code)
    .bind(&input.school_name)
    .bind(input.school_year)
    .bind(&input.enrollment_status)
    .bind(&input.employment_status)
    .bind(&input.major_group)
    .bind(input.income_bracket)
    .bind(input.kosaf_support_bracket)
    .bind(&input.housing_type)
    .bind(input.household_size)
    .bind(input.has_disability)
    .bind(input.is_multicultural_family)
    .bind(input.is_low_income_household)
    .bind(input.veteran_family)
    .bind(input.preferred_categories.clone().unwrap_or_default())
    .bind(&input.school_type)
    .bind(&input.age_band)
    .bind(nickname)
    .bind(input.homeless_household)
    .bind(&input.marital_status)
    .bind(input.newlywed)
    .bind(input.children_count)
    .bind(&input.income_median_band)
    .bind(input.birth_date)
    .bind(birth_date_sent)
    .bind(present[0])
    .bind(present[1])
    .bind(present[2])
    .bind(present[3])
    .bind(&input.telecom_carrier)
    .bind(&input.card_issuers)
    .bind(&input.pay_apps)
    .bind(present[4])
    .bind(present[5])
    .bind(present[6])
    .bind(present[7])
    .bind(present[8])
    .bind(present[9])
    .bind(present[10])
    .bind(present[11])
    .execute(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    // Fetch back the saved profile
    let profile =
        sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "user_id": user_id,
        "profile": profile
    })))
}

/// GET /api/v1/profile
///
/// Fetches the authenticated user's own profile using JWT identity.
pub async fn get_my_profile(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;

    let profile =
        sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&pool)
            .await
            .map_err(crate::errors::internal_error)?;

    match profile {
        Some(p) => Ok(Json(json!({
            "user_id": user_id,
            "profile": p
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Profile not found" })),
        )),
    }
}

/// GET /api/v1/profile/{user_id}
///
/// Fetches a profile by user_id from the path. Only the authenticated user
/// may fetch their own profile — other user IDs are rejected with 403.
pub async fn get_profile(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Enforce ownership: callers may only read their own profile
    if auth_user.id != user_id && auth_user.role != "admin" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Forbidden: you may only access your own profile" })),
        ));
    }

    let profile =
        sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&pool)
            .await
            .map_err(crate::errors::internal_error)?;

    match profile {
        Some(p) => Ok(Json(json!({
            "user_id": user_id,
            "profile": p
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Profile not found" })),
        )),
    }
}

/// A `YYYY-MM-DD` calendar date.
fn parse_date(value: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

// ── Request types ──

#[derive(Debug, serde::Deserialize)]
pub struct SaveProfileRequest {
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(flatten)]
    pub profile: ProfileInput,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review2-client M-1: every `ProfileInput` field is either base profile
    /// or kept when absent, so a field added later cannot silently take the
    /// "absent clears it" rule. `birth_date` has its own rule.
    #[test]
    fn every_profile_field_has_a_save_rule() {
        let input: ProfileInput =
            serde_json::from_value(json!({"birth_year": 2000, "region_code": "busan"})).unwrap();
        let fields: Vec<String> = serde_json::to_value(&input)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let unclassified: Vec<&String> = fields
            .iter()
            .filter(|f| {
                !BASE_PROFILE.contains(&f.as_str())
                    && !KEEP_WHEN_ABSENT.contains(&f.as_str())
                    && !["birth_date", "newlywed"].contains(&f.as_str())
            })
            .collect();
        assert!(unclassified.is_empty(), "{unclassified:?}");
        for key in BASE_PROFILE.iter().chain(&KEEP_WHEN_ABSENT) {
            assert!(
                fields.iter().any(|f| f == key),
                "{key} is no ProfileInput field"
            );
        }
        assert!(!BASE_PROFILE.iter().any(|f| KEEP_WHEN_ABSENT.contains(f)));
    }
}
