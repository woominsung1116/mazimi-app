use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    Json,
};
use mazimi_core::models::{ProfileInput, RecommendScope, RecommendationResult, UserProfile};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;

use crate::auth::AuthUser;
use crate::services::application_preparation::Error;

/// `?scope=youth` (default) lists youth-relevant rows; `?scope=all` is the
/// "전체 혜택" mode. Any other value is rejected with 400 by the extractor.
#[derive(Debug, Default, Deserialize)]
pub struct PreviewQuery {
    #[serde(default)]
    pub scope: RecommendScope,
}

/// POST /api/v1/recommend/preview: a personalised list from conditions in
/// the body, without a login. Off → 401 `login_required`.
pub async fn preview(
    State(pool): State<PgPool>,
    State(crate::AnonymousPreview(enabled)): State<crate::AnonymousPreview>,
    Query(query): Query<PreviewQuery>,
    Json(input): Json<ProfileInput>,
) -> Result<Json<RecommendationResult>, Error> {
    if !enabled {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "로그인하고 조건을 저장하면 맞춤 추천을 볼 수 있어요.",
                "code": "login_required"
            })),
        ));
    }
    crate::services::profile_validation::check_city_code(&input)?;
    crate::services::recommendations::preview(&pool, input, query.scope)
        .await
        .map(Json)
}

/// GET /api/v1/recommendations: the list for the signed-in user's saved
/// profile. The client sends no conditions; without a saved birth year and
/// region there is nothing to personalise (409 `profile_required`).
pub async fn mine(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Query(query): Query<PreviewQuery>,
) -> Result<(HeaderMap, Json<RecommendationResult>), Error> {
    let profile =
        sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id = $1")
            .bind(auth_user.id)
            .fetch_optional(&pool)
            .await
            .map_err(crate::errors::internal_error)?;
    let input = profile
        .and_then(crate::services::benefit_assistant::bundles::profile_input)
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "맞춤 추천을 보려면 조건을 먼저 저장해 주세요.",
                    "code": "profile_required"
                })),
            )
        })?;
    let result = crate::services::recommendations::preview(&pool, input, query.scope).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
    Ok((headers, Json(result)))
}
