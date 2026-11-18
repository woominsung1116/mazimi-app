use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    Json,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;

// ── Token lifetimes ──
//
// Access token: short-lived (30 minutes) — used for API authentication.
// Refresh token: long-lived (30 days) — used to obtain new access tokens
// without re-authenticating via Kakao. Every refresh token is recorded in
// `refresh_tokens` by its `jti`, so logout and rotation can revoke it before
// the JWT expiry.
const ACCESS_TOKEN_EXPIRY_SECS: usize = 30 * 60; // 1800 seconds
const REFRESH_TOKEN_EXPIRY_SECS: i64 = 30 * 24 * 60 * 60; // 2592000 seconds

// ── Claims stored inside the JWT ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: stringified user UUID
    pub sub: String,
    pub role: String,
    /// Token type: "access" or "refresh"
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// Expiry (Unix timestamp)
    pub exp: usize,
    /// Issued-at (Unix timestamp)
    pub iat: usize,
    /// Refresh-token id (row key in `refresh_tokens`). Absent on access tokens
    /// and on refresh tokens issued before server-side revocation existed;
    /// those legacy refresh tokens are rejected at `/auth/refresh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
}

fn default_token_type() -> String {
    "access".to_string()
}

// ── Extracted identity available to handlers ──

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: Uuid,
    pub role: String,
}

// ── AdminUser extractor — rejects non-admin callers with 403 ──
//
// Add `admin: AdminUser` to any handler signature to enforce role=admin.
// The middleware has already injected AuthUser into extensions; this extractor
// simply reads it and checks the role, returning 403 Forbidden if the role is
// anything other than "admin".

#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = (StatusCode, Json<serde_json::Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth_user = AuthUser::from_request_parts(parts, state).await?;

        if auth_user.role != "admin" {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Forbidden: admin access required" })),
            ));
        }

        Ok(AdminUser(auth_user))
    }
}

// ── Token helpers ──

/// Create a short-lived access token (30 min).
pub fn create_token(user_id: Uuid, role: &str, secret: &str) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp() as usize;
    sign(
        Claims {
            sub: user_id.to_string(),
            role: role.to_string(),
            token_type: "access".to_string(),
            iat: now,
            exp: now + ACCESS_TOKEN_EXPIRY_SECS,
            jti: None,
        },
        secret,
    )
}

/// Issue a long-lived refresh token (30 days) and record it in
/// `refresh_tokens` so `/auth/refresh` can rotate it and `/auth/logout` can
/// revoke it. Accepts a pool or an open transaction.
pub async fn issue_refresh_token<'e, E>(
    executor: E,
    user_id: Uuid,
    role: &str,
    secret: &str,
) -> anyhow::Result<String>
where
    E: sqlx::PgExecutor<'e>,
{
    let jti = Uuid::new_v4();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(REFRESH_TOKEN_EXPIRY_SECS);
    sqlx::query("INSERT INTO refresh_tokens (jti, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(jti)
        .bind(user_id)
        .bind(expires_at)
        .execute(executor)
        .await?;
    sign(
        Claims {
            sub: user_id.to_string(),
            role: role.to_string(),
            token_type: "refresh".to_string(),
            iat: now.timestamp() as usize,
            exp: expires_at.timestamp() as usize,
            jti: Some(jti.to_string()),
        },
        secret,
    )
}

fn sign(claims: Claims, secret: &str) -> anyhow::Result<String> {
    let token = encode(
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;
    Ok(token)
}

pub fn verify_token(token: &str, secret: &str) -> anyhow::Result<Claims> {
    let validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}

/// The user of a valid Bearer access token, if the request carries one. For
/// public routes that personalise without requiring a login: a missing,
/// malformed or expired token reads as anonymous, never as an error.
pub fn bearer_user(headers: &axum::http::HeaderMap, secret: &str) -> Option<Uuid> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let claims = verify_token(token, secret).ok()?;
    if claims.token_type != "access" {
        return None;
    }
    Uuid::parse_str(&claims.sub).ok()
}

// ── Axum extractor: reads Bearer token from Authorization header ──
//
// Axum 0.8 supports `async fn` directly in `impl FromRequestParts` without
// the `#[async_trait]` macro.

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = (StatusCode, Json<serde_json::Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(user) = parts.extensions.get::<AuthUser>() {
            return Ok(user.clone());
        }
        let unauthorized = |msg: &str| (StatusCode::UNAUTHORIZED, Json(json!({ "error": msg })));

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| unauthorized("Missing Authorization header"))?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| unauthorized("Authorization header must use Bearer scheme"))?;

        let claims = verify_token(token, &state.jwt_secret)
            .map_err(|_| unauthorized("Invalid or expired token"))?;

        if claims.token_type != "access" {
            return Err(unauthorized("Token type must be 'access'"));
        }

        let user_id =
            Uuid::parse_str(&claims.sub).map_err(|_| unauthorized("Malformed token subject"))?;

        let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(crate::errors::internal_error)?
            .ok_or_else(|| unauthorized("Account no longer exists"))?;
        Ok(AuthUser { id: user_id, role })
    }
}
