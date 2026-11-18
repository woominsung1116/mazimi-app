//! Public compatibility check. Provider identity is not compatibility evidence.
//!
//! Deprecated: every pair is `unknown` and no first-party client calls it.
//! The response shape is kept until the sunset date; responses carry
//! `Deprecation` and `Sunset` headers (RFC 9745 / RFC 8594).
use crate::services::{application_preparation::Error, benefit_comparison};
use axum::{
    extract::State,
    http::{HeaderName, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

pub const SUNSET: &str = "Sun, 31 Jan 2027 00:00:00 GMT";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackCheckRequest {
    pub program_ids: Vec<Uuid>,
}

pub async fn stack_check(
    State(pool): State<PgPool>,
    Json(body): Json<StackCheckRequest>,
) -> Response {
    let mut response = match benefit_comparison::stack_check(&pool, &body.program_ids).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => <Error as IntoResponse>::into_response(error),
    };
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("deprecation"),
        HeaderValue::from_static("true"),
    );
    headers.insert(
        HeaderName::from_static("sunset"),
        HeaderValue::from_static(SUNSET),
    );
    response
}
