use crate::{
    auth::AdminUser,
    errors::internal_error,
    routes::{agent::client_caps, programs::PROGRAM_COLUMNS},
    services::{
        application_resources::Error,
        form_templates::{self, DerivedUpload, Put},
    },
};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use mazimi_core::{form_templates::DATE_PARTS_CAP, models::Program};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;
type ResponseError = (StatusCode, Json<Value>);

pub async fn admin_get(
    _: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ResponseError> {
    form_templates::admin_get(&pool, id)
        .await
        .map(Json)
        .map_err(Error::response)
}
pub async fn admin_put(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<Put>,
) -> Result<Json<Value>, ResponseError> {
    form_templates::admin_put(&pool, admin.0.id, id, input)
        .await
        .map(Json)
        .map_err(Error::response)
}
/// Raw PDF converted from the resource's verified HWP/HWPX. The query names
/// the official file it was converted from and the converter.
pub async fn admin_put_derived_pdf(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Query(meta): Query<DerivedUpload>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ResponseError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/pdf")
    {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(
                json!({"error":"application/pdf 본문만 받을 수 있습니다.","code":"invalid_resource"}),
            ),
        ));
    }
    form_templates::admin_put_derived_pdf(&pool, admin.0.id, id, meta, &body)
        .await
        .map(Json)
        .map_err(Error::response)
}
pub async fn admin_delete(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ResponseError> {
    form_templates::admin_delete(&pool, admin.0.id, id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(Error::response)
}

fn not_found() -> ResponseError {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":"양식 입력 칸 정보가 없습니다.","code":"form_template_not_found"})),
    )
}

/// Same visibility as GET /programs/{id}.
async fn visible_program(pool: &PgPool, id: Uuid) -> Result<Program, ResponseError> {
    sqlx::query_as::<_, Program>(&format!(
        "SELECT {PROGRAM_COLUMNS} \
         FROM current_programs WHERE id = $1 AND is_active = true AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now())"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?
    .ok_or_else(not_found)
}

/// Public: input box positions for a currently published template.
/// 404 means "no definition": the app falls back to the form's own fields
/// or the guided copy sheet.
///
/// Date-part boxes go only to a client that says it reads them
/// (`X-Mazimi-Client-Caps: form-date-parts-v1`); any other client gets them
/// as plain boxes with no mapping and no part, exactly as before parts
/// existed, so one stored revision serves old and new builds. The two answers
/// differ, so the ETag carries the cap and the response varies on the header:
/// a cached copy never crosses from one kind of client to the other.
pub async fn public_get(
    State(pool): State<PgPool>,
    Path((id, resource)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Response, ResponseError> {
    let program = visible_program(&pool, id).await?;
    let template = form_templates::public_get(&pool, &program, resource)
        .await
        .map_err(Error::response)?
        .ok_or_else(not_found)?;
    let parts = client_caps(&headers).any(|c| c == DATE_PARTS_CAP);
    let template = if parts {
        template
    } else {
        template.for_clients_without_date_parts()
    };
    let body = serde_json::to_vec(&template).map_err(internal_error)?;
    let digest = format!("{:x}", Sha256::digest(&body));
    let etag = HeaderValue::from_str(&format!(
        "\"{}{}\"",
        &digest[..32],
        if parts { "-dp1" } else { "" }
    ))
    .map_err(internal_error)?;
    let cache = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        (
            header::VARY,
            HeaderValue::from_static("X-Mazimi-Client-Caps"),
        ),
    ];
    if etag_matches(&headers, &etag) {
        return Ok((StatusCode::NOT_MODIFIED, cache).into_response());
    }
    Ok((
        cache,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// Public: the blank PDF Mazimi converted from a published HWP/HWPX form.
/// The app verifies its sha256 against the template's `content_hash`.
/// `If-None-Match` with the current hash answers 304 before the bytes are
/// read; the bytes for one hash never change, so they may be cached.
pub async fn public_derived_pdf(
    State(pool): State<PgPool>,
    Path((id, resource)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Response, ResponseError> {
    let program = visible_program(&pool, id).await?;
    let hash = form_templates::public_derived_pdf_hash(&pool, &program, resource)
        .await
        .map_err(Error::response)?
        .ok_or_else(not_found)?;
    let etag = HeaderValue::from_str(&format!("\"{hash}\"")).map_err(internal_error)?;
    let cache = HeaderValue::from_static("public, max-age=86400, immutable");
    if etag_matches(&headers, &etag) {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache)],
        )
            .into_response());
    }
    let pdf = form_templates::derived_pdf_bytes(&pool, resource, &hash)
        .await
        .map_err(Error::response)?
        .ok_or_else(not_found)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/pdf"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"form.pdf\""),
            ),
            (header::CACHE_CONTROL, cache),
            (header::ETAG, etag),
        ],
        pdf,
    )
        .into_response())
}

/// `If-None-Match` names this entity tag (weak or strong), or is `*`.
fn etag_matches(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Ok(etag) = etag.to_str() else {
        return false;
    };
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.trim_start_matches("W/") == etag)
}
