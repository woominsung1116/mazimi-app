use axum::{body::Bytes, extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{auth::AuthUser, AppState};

// ── Request types ──

#[derive(Debug, Deserialize)]
pub struct RegisterPushTokenRequest {
    pub token: String,
    pub platform: String,
}

#[derive(Debug, Deserialize)]
pub struct UnregisterPushTokenRequest {
    pub token: String,
}

fn valid_expo_push_token(token: &str) -> bool {
    // Expo supports both bracketed tokens and the legacy UUID-shaped form:
    // https://github.com/expo/expo-server-sdk-node/blob/main/src/ExpoClient.ts
    // The length cap is our request/storage limit, not an Expo token guarantee.
    if token.len() > 256 {
        return false;
    }
    if let Some(inner) = token
        .strip_prefix("ExpoPushToken[")
        .or_else(|| token.strip_prefix("ExponentPushToken["))
        .and_then(|value| value.strip_suffix(']'))
    {
        return !inner.is_empty()
            && inner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    }
    let parts: Vec<_> = token.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(part, size)| {
            part.len() == size && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

// Devices per account. A re-registered device refreshes its own row; beyond
// the cap the least recently registered tokens are dropped, which bounds
// notification fan-out and storage for one account.
pub const MAX_PUSH_TOKENS_PER_USER: i64 = 10;

// Matches the `push_tokens.platform` CHECK (20260321000011). Expo push tokens
// only exist for native builds; the web export never obtains one, so "web"
// is rejected here instead of failing the CHECK with a 500.
const VALID_PLATFORMS: &[&str] = &["ios", "android"];

// A token registered to another account moves only once that row is stale:
// not re-registered for this long. The app re-registers on every launch with
// a session, and a session lives at most 30 days without use (the refresh
// token lifetime, auth.rs), so a stale row's account has no live session on
// that device. Logout deletes the device's row, so a normal account switch on
// one phone never needs a takeover.
pub const PUSH_TOKEN_STALE_DAYS: i32 = 30;

// ── Handlers ──

/// POST /api/v1/push/register
///
/// Upserts an Expo push token for the authenticated user/device pair.
/// The user's ID is taken from the JWT — no client-supplied user_id accepted.
/// A conflict on `token` (unique) refreshes the row when it is the same
/// user's, or moves it to this user when the row is stale
/// ([`PUSH_TOKEN_STALE_DAYS`]). A live token of another account is refused
/// with 409, so knowing someone's token cannot silence their alerts.
/// Then the account is trimmed to `MAX_PUSH_TOKENS_PER_USER` newest tokens.
pub async fn register_push_token(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<RegisterPushTokenRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;

    if !valid_expo_push_token(&body.token) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid Expo push token" })),
        ));
    }

    if !VALID_PLATFORMS.contains(&body.platform.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid platform" })),
        ));
    }

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(crate::errors::internal_error)?;
    let written = sqlx::query(
        r#"
        INSERT INTO push_tokens (user_id, token, platform, created_at, updated_at)
        VALUES ($1, $2, $3, now(), now())
        ON CONFLICT (token) DO UPDATE
            SET user_id    = EXCLUDED.user_id,
                platform   = EXCLUDED.platform,
                updated_at = now()
            WHERE push_tokens.user_id = EXCLUDED.user_id
               OR push_tokens.updated_at < now() - make_interval(days => $4)
        "#,
    )
    .bind(user_id)
    .bind(&body.token)
    .bind(&body.platform)
    .bind(PUSH_TOKEN_STALE_DAYS)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    if written.rows_affected() == 0 {
        // The token belongs to another account and is not stale.
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "Push token is already registered" })),
        ));
    }
    sqlx::query(
        "DELETE FROM push_tokens WHERE id IN ( \
            SELECT id FROM push_tokens WHERE user_id = $1 \
            ORDER BY updated_at DESC, created_at DESC, id DESC OFFSET $2)",
    )
    .bind(user_id)
    .bind(MAX_PUSH_TOKENS_PER_USER)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(json!({ "success": true })))
}

/// DELETE /api/v1/push/register
///
/// With a `{ token }` body, removes only that device's token so logging out
/// on one phone keeps the user's other devices subscribed. Without a body,
/// removes all push tokens for the authenticated user.
/// The user's ID is taken from the JWT — no client-supplied user_id accepted.
pub async fn unregister_push_token(
    auth_user: AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;
    // Clients send a JSON content type even without a body, so an empty body
    // means "all tokens" rather than a parse error.
    let body = if body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<UnregisterPushTokenRequest>(&body).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Invalid request body" })),
                )
            })?,
        )
    };

    match body {
        Some(body) => {
            sqlx::query("DELETE FROM push_tokens WHERE user_id = $1 AND token = $2")
                .bind(user_id)
                .bind(&body.token)
                .execute(&state.pool)
                .await
        }
        None => {
            sqlx::query("DELETE FROM push_tokens WHERE user_id = $1")
                .bind(user_id)
                .execute(&state.pool)
                .await
        }
    }
    .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({ "success": true })))
}

#[cfg(test)]
mod tests {
    use super::valid_expo_push_token;

    #[test]
    fn push_token_accepts_current_and_legacy_forms_but_not_unbounded_input() {
        for token in [
            "ExpoPushToken[abcDEF012_-]",
            "ExponentPushToken[abcDEF012_-]",
            "12345678-abcd-ABCD-0123-1234567890ab",
        ] {
            assert!(valid_expo_push_token(token));
        }
        for token in [
            "",
            "arbitrary-token",
            "ExpoPushToken[]",
            "ExpoPushToken[a b]",
            "ExpoPushToken[a\n]",
            "ExpoPushToken[[abc]]",
            "ExpoPushToken[한글]",
        ] {
            assert!(!valid_expo_push_token(token));
        }
        assert!(!valid_expo_push_token(&format!(
            "ExpoPushToken[{}]",
            "a".repeat(256)
        )));
    }
}
