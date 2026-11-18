use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
type Error = (StatusCode, Json<Value>);

use crate::services::profile_validation::resolve_university;

pub async fn universities(State(pool): State<PgPool>) -> Result<Json<Value>, Error> {
    let items = sqlx::query_as::<_, (String, String, Option<String>, Vec<String>)>(
        "SELECT id,name,region,aliases FROM universities WHERE is_active ORDER BY region,name",
    )
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Ok(Json(
        json!({"items":items.into_iter().map(|(id,name,region,aliases)|json!({"id":id,"name":name,"region":region,"aliases":aliases})).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
pub struct CampusQuery {
    /// A guest's school, picked at onboarding and kept on the device. Ignored
    /// for a signed-in member (see [`viewer`]), who may omit it.
    #[serde(default)]
    school: String,
    q: Option<String>,
    enrollment_status: Option<String>,
    page: Option<i64>,
}

/// What the 대학 tab shows the caller.
enum Viewer {
    /// A recognised school (`universities.id`): its own offers plus the
    /// common ones.
    School(String),
    /// A member whose profile names no recognised school: the common offers
    /// only, flagged `school_required` so the app can say a school unlocks
    /// the school-only offers.
    MemberWithoutSchool,
    /// A guest naming a school that is no university: nothing.
    UnknownGuestSchool,
}

/// A signed-in member's school is the one saved in their profile
/// (`user_profiles.school_name`) and any `school` parameter is ignored, so a
/// member cannot browse another school's student-only offers by naming it.
/// Only a guest, whose school lives on the device, names it with the
/// parameter. An invalid or expired token is a guest, as on the catalogue
/// list. Either way the name must resolve to a real university: a mistyped
/// school unlocks no school's offers. Membership itself is self-reported.
async fn viewer(
    pool: &PgPool,
    secret: &crate::JwtSecret,
    headers: &HeaderMap,
    param: &str,
) -> Result<Viewer, Error> {
    let (school, unresolved) = match crate::auth::bearer_user(headers, &secret.0) {
        Some(user) => (
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT school_name FROM user_profiles WHERE user_id = $1",
            )
            .bind(user)
            .fetch_optional(pool)
            .await
            .map_err(crate::errors::internal_error)?
            .flatten(),
            Viewer::MemberWithoutSchool,
        ),
        None => {
            let param = param.trim();
            if param.is_empty() || param.chars().count() > 200 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"학교를 선택해 주세요."})),
                ));
            }
            (Some(param.to_owned()), Viewer::UnknownGuestSchool)
        }
    };
    let Some(school) = school.filter(|s| !s.trim().is_empty()) else {
        return Ok(unresolved);
    };
    Ok(resolve_university(pool, school.trim())
        .await?
        .map_or(unresolved, |(id, _)| Viewer::School(id)))
}

pub async fn programs(
    State(pool): State<PgPool>,
    State(secret): State<crate::JwtSecret>,
    headers: HeaderMap,
    Query(query): Query<CampusQuery>,
) -> Result<(HeaderMap, Json<Value>), Error> {
    // The answer depends on the token: never cached across viewers.
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    // `None` binds NULL: `university_id = NULL` matches no row, so only the
    // common offers are selected.
    let school_id = match viewer(&pool, &secret, &headers, &query.school).await? {
        Viewer::School(id) => Some(id),
        Viewer::MemberWithoutSchool => None,
        Viewer::UnknownGuestSchool => {
            return Ok((
                response_headers,
                Json(
                    json!({"total":0,"items":[],"school_total":0,"common_total":0,"school_required":true,"page":1,"per_page":100,"has_next":false}),
                ),
            ))
        }
    };
    let school_required = school_id.is_none();
    // Common offers retain their corporate program ID, source monitor and
    // application history. Never clone them once per university.
    let page = query.page.unwrap_or(1).clamp(1, 10_000);
    let search = super::programs::search_text(query.q.as_deref());
    // Mobile sends "재학"/"휴학"; stored payloads list canonical codes next to
    // the Korean alias. Compare on the canonical code so both vocabularies
    // select the same offers.
    let enrollment =
        crate::services::profile_validation::enrollment_status(query.enrollment_status.as_deref())?;
    // The school's own offers (mazimi_core::categories::school_offer_sql: the
    // one place a campus offer is listed) and the companies' offers for every
    // university student.
    let campus = mazimi_core::categories::campus_offer_sql("p");
    let selection = format!(
        "FROM current_programs p
         WHERE {available} AND (
           {school_offer}
           OR (p.program_type='corporate_benefit' AND p.normalized_payload->>'audience_scope'='{higher_education}'))
           AND ($2::text IS NULL OR {search})
           AND ($3::text IS NULL OR p.normalized_payload->'enrollment_statuses' IS NULL OR p.normalized_payload->'enrollment_statuses'='null'::jsonb OR (p.normalized_payload->'enrollment_statuses') ? $3)",
        available = mazimi_core::currently_available_sql("p.", "now()"),
        school_offer = mazimi_core::categories::school_offer_sql("p", "$1"),
        higher_education = mazimi_core::structural::HIGHER_EDUCATION,
        // The catalogue's search, wildcards literal (audit L6).
        search = super::programs::search_sql("$2", "p."),
    );
    let (total, school_total): (i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*), count(*) FILTER(WHERE {campus}) {selection}"
    ))
    .bind(&school_id)
    .bind(&search)
    .bind(enrollment)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let items = sqlx::query_as::<_, mazimi_core::models::Program>(&format!(
        "SELECT p.* {selection} ORDER BY ({campus}) DESC,p.title,p.id LIMIT 100 OFFSET $4"
    ))
    .bind(school_id)
    .bind(search)
    .bind(enrollment)
    .bind((page - 1) * 100)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Ok((
        response_headers,
        Json(
            json!({"total":total,"school_total":school_total,"common_total":total-school_total,"school_required":school_required,"items":items,"page":page,"per_page":100,"has_next":page*100<total}),
        ),
    ))
}
