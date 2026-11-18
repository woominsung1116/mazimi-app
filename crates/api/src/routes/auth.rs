use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Redirect,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::{create_token, issue_refresh_token, verify_token, AuthUser},
    AppState,
};

// ── Kakao API response shape (minimum fields we need) ──

#[derive(Debug, serde::Deserialize)]
struct KakaoUserMe {
    id: i64,
    kakao_account: Option<KakaoAccount>,
}

#[derive(Debug, serde::Deserialize)]
struct KakaoAccount {
    profile: Option<KakaoProfile>,
}

#[derive(Debug, serde::Deserialize)]
struct KakaoProfile {
    nickname: Option<String>,
    profile_image_url: Option<String>,
}

// Sign-in happens only through the server-side OAuth handoff below
// (`/auth/{provider}/start` → `/callback` → `/exchange`). The former
// `POST /auth/kakao`, which accepted a client-supplied Kakao access token,
// was removed: no client used it and it never checked the token's app_id.

/// GET /api/v1/auth/me
///
/// Returns the authenticated user's basic info. JWT is already verified by
/// the auth middleware; `AuthUser` is injected via request extensions.
pub async fn me(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let err_not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
    };

    let row = sqlx::query_as::<
        _,
        (
            uuid::Uuid,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            bool,
        ),
    >(
        r#"
        SELECT u.id, u.email, u.role, up.nickname, up.profile_image_url,
               (up.birth_year IS NOT NULL) AS profile_complete
        FROM users u
        LEFT JOIN user_profiles up ON up.user_id = u.id
        WHERE u.id = $1
        "#,
    )
    .bind(auth_user.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(err_not_found)?;

    Ok(Json(json!({
        "id":               row.0,
        "email":            row.1,
        "nickname":         row.3,
        "image":            row.4,
        "role":             row.2,
        "profile_complete": row.5,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteAccountRequest {
    confirmation: String,
}

/// Delete only the authenticated account. Personal rows cascade in one DB
/// statement; public programmes and other users' applications are untouched.
pub async fn delete_account(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<DeleteAccountRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.confirmation != "회원 탈퇴" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"회원 탈퇴 확인이 필요합니다"})),
        ));
    }
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth_user.id)
        .execute(&state.pool)
        .await
        .map_err(crate::errors::internal_error)?;
    Ok(Json(json!({"deleted":true})))
}

// ── Refresh token ──

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Validate a refresh JWT and return `(user_id, jti)`. Tokens without a
/// `jti` predate server-side revocation and cannot be revoked, so they are
/// rejected like any other invalid token.
fn refresh_claims(token: &str, secret: &str) -> Result<(Uuid, Uuid), AuthError> {
    let invalid = || auth_error(StatusCode::UNAUTHORIZED, "Invalid or expired refresh token");
    let claims = verify_token(token, secret).map_err(|_| invalid())?;
    if claims.token_type != "refresh" {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "Token is not a refresh token",
        ));
    }
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| auth_error(StatusCode::UNAUTHORIZED, "Malformed token subject"))?;
    let jti = claims
        .jti
        .as_deref()
        .and_then(|jti| Uuid::parse_str(jti).ok())
        .ok_or_else(invalid)?;
    Ok((user_id, jti))
}

/// How long after a revocation the revoked token may be presented again
/// without being read as theft. A client whose response was lost (timeout,
/// app killed between the server commit and token storage) retries with the
/// old token well inside this window, and a client whose sign-out and a
/// concurrent 401 retry overlap presents a just-revoked token too; a stolen
/// token replayed later is not. Inside the window the request is refused
/// (401) and, when the successor is still unused, the chain is continued
/// instead; only reuse outside the window revokes every session.
const REFRESH_ROTATION_GRACE_SECS: f64 = 60.0;

/// A rotated-away token presented again inside the grace window, whose
/// successor was never used, is a lost response: revoke the unused successor
/// and return its `jti` so the new pair chains from it. Every other reuse
/// (outside the window, revoked by logout, successor already used) yields
/// `None`. One statement, so two concurrent retries cannot both reclaim.
///
/// Trade-off: a thief replaying inside the window gets one pair without the
/// account-wide revocation, but the legitimate client's next refresh (30
/// minutes later, outside the window) still trips it. A presentation whose
/// successor was already used revokes that token's descendant chain instead.
async fn reclaim_lost_rotation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    jti: Uuid,
) -> Result<Option<Uuid>, AuthError> {
    sqlx::query_scalar::<_, Uuid>(
        "UPDATE refresh_tokens next SET revoked_at = now() \
         FROM refresh_tokens prev \
         WHERE prev.jti = $1 AND prev.user_id = $2 \
           AND prev.revoked_at > now() - make_interval(secs => $3) \
           AND next.jti = prev.replaced_by AND next.user_id = prev.user_id \
           AND next.revoked_at IS NULL AND next.replaced_by IS NULL \
           AND next.expires_at > now() \
         RETURNING next.jti",
    )
    .bind(jti)
    .bind(user_id)
    .bind(REFRESH_ROTATION_GRACE_SECS)
    .fetch_optional(&mut **tx)
    .await
    .map_err(crate::errors::internal_error)
}

/// POST /api/v1/auth/refresh
///
/// Exchanges a refresh token for a new access + refresh pair exactly once.
/// The presented token is revoked atomically. A revoked token presented
/// inside [`REFRESH_ROTATION_GRACE_SECS`] is a lost response or an overlap
/// with sign-out, not theft: the chain continues when the successor is still
/// unused (see [`reclaim_lost_rotation`]) and is otherwise refused with 401;
/// a rotated token's descendant chain is revoked, every other session stays. A revoked token
/// presented after the window is treated as theft and revokes every refresh
/// token of that account.
pub async fn refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<Value>, AuthError> {
    let (user_id, jti) = refresh_claims(&body.refresh_token, &state.jwt_secret)?;

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(crate::errors::internal_error)?;
    let claimed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE jti = $1 AND user_id = $2 AND revoked_at IS NULL AND expires_at > now() \
         RETURNING user_id",
    )
    .bind(jti)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;

    // The row whose `replaced_by` will point at the pair issued below.
    let chain_jti = if claimed.is_some() {
        jti
    } else if let Some(successor) = reclaim_lost_rotation(&mut tx, user_id, jti).await? {
        tracing::warn!(
            user_id = %user_id,
            "rotated refresh token retried inside the grace window; chain continued"
        );
        successor
    } else {
        // `(revoked, revoked inside the grace window)` of the presented row.
        // Expired or never-issued tokens have no row or no revocation and are
        // simply refused.
        let revocation = sqlx::query_as::<_, (bool, bool)>(
            "SELECT revoked_at IS NOT NULL, \
                    revoked_at IS NOT NULL AND revoked_at > now() - make_interval(secs => $3) \
             FROM refresh_tokens WHERE jti = $1 AND user_id = $2",
        )
        .bind(jti)
        .bind(user_id)
        .bind(REFRESH_ROTATION_GRACE_SECS)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
        match revocation {
            Some((true, true)) => {
                // Revoked seconds ago. By logout (`replaced_by` NULL): the
                // client's own retry overlapping sign-out, nothing to do.
                // By rotation with the successor already used: either a
                // second retry of a lost response or a thief who rotated
                // first. Clients refresh single-flight, so revoke the chain
                // this token started (security L1); other devices stay.
                let revoked = sqlx::query(
                    "WITH RECURSIVE chain(jti) AS ( \
                         SELECT replaced_by FROM refresh_tokens \
                         WHERE jti = $1 AND user_id = $2 AND replaced_by IS NOT NULL \
                         UNION \
                         SELECT r.replaced_by FROM refresh_tokens r JOIN chain c ON r.jti = c.jti \
                         WHERE r.user_id = $2 AND r.replaced_by IS NOT NULL) \
                     UPDATE refresh_tokens SET revoked_at = now() \
                     WHERE user_id = $2 AND revoked_at IS NULL AND jti IN (SELECT jti FROM chain)",
                )
                .bind(jti)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(crate::errors::internal_error)?
                .rows_affected();
                tx.commit().await.map_err(crate::errors::internal_error)?;
                tracing::warn!(
                    user_id = %user_id,
                    revoked,
                    "refresh token revoked inside the grace window presented again; its rotation chain revoked, other sessions kept"
                );
            }
            Some((true, false)) => {
                sqlx::query(
                    "UPDATE refresh_tokens SET revoked_at = now() \
                     WHERE user_id = $1 AND revoked_at IS NULL",
                )
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(crate::errors::internal_error)?;
                tx.commit().await.map_err(crate::errors::internal_error)?;
                tracing::warn!(
                    user_id = %user_id,
                    "revoked refresh token presented again outside the grace window; all sessions revoked"
                );
            }
            _ => {}
        }
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "Invalid or expired refresh token",
        ));
    };

    // Re-fetch the current role from DB instead of trusting the old token's role
    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?
        .ok_or_else(|| auth_error(StatusCode::UNAUTHORIZED, "Account no longer exists"))?;

    let access_token = create_token(user_id, &role, &state.jwt_secret).map_err(|e| {
        tracing::error!(error = %e, "Token creation failed in refresh");
        auth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            crate::errors::GENERIC_ERROR_MESSAGE,
        )
    })?;
    let new_refresh = issue_refresh_token(&mut *tx, user_id, &role, &state.jwt_secret)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Refresh token creation failed in refresh");
            auth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::errors::GENERIC_ERROR_MESSAGE,
            )
        })?;
    let (_, new_jti) = refresh_claims(&new_refresh, &state.jwt_secret)?;
    sqlx::query("UPDATE refresh_tokens SET replaced_by = $2 WHERE jti = $1")
        .bind(chain_jti)
        .bind(new_jti)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "token": access_token,
        "refresh_token": new_refresh
    })))
}

/// POST /api/v1/auth/logout
///
/// Revokes the presented refresh token server-side. The refresh token is the
/// credential: its signature, `token_type` and `jti` prove possession, so no
/// access token is required and a client whose access token already expired
/// can still end its session. Only that one `jti` of the claimed user is
/// revoked. Idempotent and uniform: an already revoked, expired, malformed or
/// unknown token changes nothing and still returns 200, so the response never
/// reveals whether a token was valid and a client can always finish signing
/// out locally.
#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
    /// This device's Expo push token. Removed with the session, because a
    /// sign-out whose access token expired cannot reach DELETE
    /// /push/register, and a row left behind would refuse the next account
    /// on the device for up to `PUSH_TOKEN_STALE_DAYS` (routes/push.rs).
    #[serde(default)]
    pub push_token: Option<String>,
}

pub async fn logout(
    State(state): State<AppState>,
    Json(body): Json<LogoutRequest>,
) -> Result<Json<Value>, AuthError> {
    if let Ok((user_id, jti)) = refresh_claims(&body.refresh_token, &state.jwt_secret) {
        sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = now() \
             WHERE jti = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(jti)
        .bind(user_id)
        .execute(&state.pool)
        .await
        .map_err(crate::errors::internal_error)?;
        if let Some(push_token) = body.push_token.as_deref().filter(|t| t.len() <= 256) {
            // Only this account's row for this device.
            sqlx::query("DELETE FROM push_tokens WHERE user_id = $1 AND token = $2")
                .bind(user_id)
                .bind(push_token)
                .execute(&state.pool)
                .await
                .map_err(crate::errors::internal_error)?;
        }
    }
    Ok(Json(json!({ "logged_out": true })))
}

// Server-side OAuth. Only a short-lived, verifier-bound code reaches the app.
type AuthError = (StatusCode, Json<Value>);
fn auth_error(status: StatusCode, message: &str) -> AuthError {
    (status, Json(json!({"error": message})))
}
fn handoff_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn random_code() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn credentials(provider: &str) -> Option<(String, String)> {
    let prefix = provider.to_ascii_uppercase();
    let id = std::env::var(format!("{prefix}_CLIENT_ID")).unwrap_or_default();
    let secret = std::env::var(format!("{prefix}_CLIENT_SECRET")).unwrap_or_default();
    let usable = |value: &str| !value.trim().is_empty() && !value.starts_with("CHANGE_ME");
    if usable(&id) && (provider == "kakao" || usable(&secret)) {
        Some((id, secret))
    } else {
        None
    }
}
pub async fn providers() -> Json<Value> {
    Json(
        json!({"kakao": credentials("kakao").is_some(), "google": credentials("google").is_some()}),
    )
}
/// Origin of the Next.js site. Caddy serves it on `DOMAIN`, the host the
/// API's CORS layer already trusts; local and test runs use the dev server.
fn site_origin(app_env: &str) -> String {
    if matches!(app_env, "local" | "test") {
        return "http://localhost:3000".into();
    }
    format!(
        "https://{}",
        std::env::var("DOMAIN")
            .unwrap_or_default()
            .trim()
            .trim_end_matches('/')
    )
}
/// `web` is the Expo web build on `APP_DOMAIN`, `site` the Next.js site
/// (its server route finishes the exchange and keeps the tokens in httpOnly
/// cookies), `mobile` the native app.
fn app_redirect(
    app_env: &str,
    client: &str,
    provider: &str,
    parameter: &str,
    value: &str,
) -> Redirect {
    let target = match client {
        "web" => format!(
            "https://{}/login",
            std::env::var("APP_DOMAIN")
                .unwrap_or_else(|_| std::env::var("DOMAIN").unwrap_or_default())
        ),
        "site" => format!("{}/auth/callback", site_origin(app_env)),
        _ => "mazimi://login".into(),
    };
    Redirect::temporary(&format!(
        "{target}?provider={provider}&{parameter}={}",
        urlencoding::encode(value)
    ))
}
#[derive(Deserialize)]
pub struct LoginStart {
    challenge: String,
    client: String,
}
pub async fn kakao_start(
    State(state): State<AppState>,
    Query(q): Query<LoginStart>,
) -> Result<Redirect, AuthError> {
    login_start(&state, q, "kakao").await
}
pub async fn google_start(
    State(state): State<AppState>,
    Query(q): Query<LoginStart>,
) -> Result<Redirect, AuthError> {
    login_start(&state, q, "google").await
}
async fn login_start(
    state: &AppState,
    q: LoginStart,
    provider: &str,
) -> Result<Redirect, AuthError> {
    use base64::Engine;
    if q.challenge.len() != 64
        || !q.challenge.bytes().all(|b| b.is_ascii_hexdigit())
        || !matches!(q.client.as_str(), "web" | "site" | "mobile")
    {
        return Err(auth_error(StatusCode::BAD_REQUEST, "Invalid login request"));
    }
    let (id, _) = credentials(provider).ok_or_else(|| {
        auth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "아직 준비 중인 로그인 방식이에요.",
        )
    })?;
    let nonce = random_code();
    let verifier = if provider == "google" {
        Some(random_code())
    } else {
        None
    };
    sqlx::query("DELETE FROM oauth_handoffs WHERE expires_at<now()")
        .execute(&state.pool)
        .await
        .map_err(crate::errors::internal_error)?;
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,provider,provider_verifier) VALUES($1,$2,$3,$4,$5)")
        .bind(handoff_hash(&nonce)).bind(q.challenge.to_ascii_lowercase()).bind(q.client)
        .bind(provider).bind(&verifier).execute(&state.pool).await.map_err(crate::errors::internal_error)?;
    let callback = format!(
        "{}/api/v1/auth/{provider}/callback",
        state.public_api_base_url
    );
    let endpoint = if provider == "google" {
        "https://accounts.google.com/o/oauth2/v2/auth"
    } else {
        "https://kauth.kakao.com/oauth/authorize"
    };
    let mut url = reqwest::Url::parse(endpoint).expect("constant OAuth URL");
    url.query_pairs_mut().extend_pairs([
        ("client_id", id.as_str()),
        ("redirect_uri", callback.as_str()),
        ("response_type", "code"),
        ("state", nonce.as_str()),
    ]);
    if let Some(verifier) = verifier {
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        url.query_pairs_mut().extend_pairs([
            ("scope", "openid profile"),
            ("prompt", "select_account"),
            ("nonce", nonce.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
        ]);
    } else {
        url.query_pairs_mut()
            .append_pair("scope", "profile_nickname,profile_image");
    }
    Ok(Redirect::temporary(url.as_str()))
}
#[derive(Deserialize)]
pub struct OAuthCallback {
    code: Option<String>,
    state: String,
    error: Option<String>,
}
pub async fn kakao_callback(
    State(state): State<AppState>,
    Query(q): Query<OAuthCallback>,
) -> Result<Redirect, AuthError> {
    callback(&state, q, "kakao").await
}
pub async fn google_callback(
    State(state): State<AppState>,
    Query(q): Query<OAuthCallback>,
) -> Result<Redirect, AuthError> {
    callback(&state, q, "google").await
}
async fn callback(
    state: &AppState,
    q: OAuthCallback,
    provider: &str,
) -> Result<Redirect, AuthError> {
    if q.state.len() != 64 {
        return Err(auth_error(
            StatusCode::BAD_REQUEST,
            "로그인을 다시 시작해 주세요.",
        ));
    }
    let state_hash = handoff_hash(&q.state);
    let (client, verifier) = sqlx::query_as::<_,(String,Option<String>)>("UPDATE oauth_handoffs SET callback_used=true WHERE state_hash=$1 AND provider=$2 AND expires_at>now() AND NOT callback_used RETURNING client,provider_verifier")
        .bind(&state_hash).bind(provider).fetch_optional(&state.pool).await.map_err(crate::errors::internal_error)?
        .ok_or_else(|| auth_error(StatusCode::BAD_REQUEST, "로그인 요청이 만료되었어요. 다시 시작해 주세요."))?;
    if q.error.is_some() || q.code.as_deref().is_none_or(str::is_empty) {
        return Ok(app_redirect(
            &state.app_env,
            &client,
            provider,
            "error",
            "cancelled",
        ));
    }
    // Never log authorization codes, provider tokens, or response bodies.
    let identity = provider_identity(
        state,
        provider,
        q.code.as_deref().unwrap_or_default(),
        &q.state,
        verifier.as_deref(),
    )
    .await;
    let identity = match identity {
        Ok(identity) => identity,
        Err(_) => {
            tracing::warn!(provider, "OAuth provider verification failed");
            return Ok(app_redirect(
                &state.app_env,
                &client,
                provider,
                "error",
                "failed",
            ));
        }
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(crate::errors::internal_error)?;
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users(auth_provider,auth_provider_id,role,created_at,updated_at) VALUES($1,$2,'user',now(),now()) ON CONFLICT(auth_provider,auth_provider_id) DO UPDATE SET updated_at=now() RETURNING id")
        .bind(provider).bind(identity.id).fetch_one(&mut *tx).await.map_err(crate::errors::internal_error)?;
    let profile_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_profiles WHERE user_id=$1 AND birth_year IS NOT NULL)",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    sqlx::query("INSERT INTO user_profiles(user_id,nickname,profile_image_url,updated_at) VALUES($1,$2,$3,now()) ON CONFLICT(user_id) DO UPDATE SET nickname=COALESCE(user_profiles.nickname,EXCLUDED.nickname),profile_image_url=COALESCE(user_profiles.profile_image_url,EXCLUDED.profile_image_url),updated_at=now()")
        .bind(user_id).bind(identity.nickname).bind(identity.image).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    let exchange = random_code();
    sqlx::query("UPDATE oauth_handoffs SET exchange_hash=$2,user_id=$3,is_new=$4,provider_verifier=NULL,expires_at=now()+interval '2 minutes' WHERE state_hash=$1")
        .bind(state_hash).bind(handoff_hash(&exchange)).bind(user_id).bind(!profile_exists)
        .execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(app_redirect(
        &state.app_env,
        &client,
        provider,
        "code",
        &exchange,
    ))
}
struct Identity {
    id: String,
    nickname: Option<String>,
    image: Option<String>,
}
async fn provider_identity(
    state: &AppState,
    provider: &str,
    code: &str,
    nonce: &str,
    verifier: Option<&str>,
) -> Result<Identity, ()> {
    let (id, secret) = credentials(provider).ok_or(())?;
    let callback = format!(
        "{}/api/v1/auth/{provider}/callback",
        state.public_api_base_url
    );
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("client_id", id.as_str()),
        ("client_secret", secret.as_str()),
        ("redirect_uri", callback.as_str()),
        ("code", code),
    ];
    let endpoint = if provider == "google" {
        params.push(("code_verifier", verifier.ok_or(())?));
        "https://oauth2.googleapis.com/token"
    } else {
        "https://kauth.kakao.com/oauth/token"
    };
    #[derive(Deserialize)]
    struct Tokens {
        access_token: String,
        id_token: Option<String>,
    }
    let tokens: Tokens = state
        .http_client
        .post(endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())?;
    if provider == "google" {
        let token = tokens.id_token.ok_or(())?;
        let keys = google_keys(state, &token).await?;
        let claims = verify_google_token(&token, &keys, &id, nonce)?;
        Ok(Identity {
            id: claims.sub,
            nickname: claims.name,
            image: claims.picture,
        })
    } else {
        let user: KakaoUserMe = state
            .http_client
            .get("https://kapi.kakao.com/v2/user/me")
            .bearer_auth(tokens.access_token)
            .send()
            .await
            .map_err(|_| ())?
            .error_for_status()
            .map_err(|_| ())?
            .json()
            .await
            .map_err(|_| ())?;
        if user.id <= 0 {
            return Err(());
        }
        let profile = user.kakao_account.and_then(|a| a.profile);
        Ok(Identity {
            id: user.id.to_string(),
            nickname: profile.as_ref().and_then(|p| p.nickname.clone()),
            image: profile.and_then(|p| p.profile_image_url),
        })
    }
}
#[derive(Debug, Deserialize)]
struct GoogleClaims {
    sub: String,
    nonce: String,
    azp: Option<String>,
    name: Option<String>,
    picture: Option<String>,
}
fn verify_google_token(
    token: &str,
    keys: &jsonwebtoken::jwk::JwkSet,
    client_id: &str,
    nonce: &str,
) -> Result<GoogleClaims, ()> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
    let header = decode_header(token).map_err(|_| ())?;
    if header.alg != Algorithm::RS256 {
        return Err(());
    }
    let key = keys.find(header.kid.as_deref().ok_or(())?).ok_or(())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&["accounts.google.com", "https://accounts.google.com"]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<GoogleClaims>(
        token,
        &DecodingKey::from_jwk(key).map_err(|_| ())?,
        &validation,
    )
    .map_err(|_| ())?
    .claims;
    if claims.nonce != nonce
        || claims.sub.is_empty()
        || claims.sub.len() > 255
        || !claims.sub.is_ascii()
        || claims.azp.as_deref().is_some_and(|azp| azp != client_id)
    {
        return Err(());
    }
    Ok(claims)
}
type CachedKeys = Option<(std::time::Instant, jsonwebtoken::jwk::JwkSet)>;
static GOOGLE_KEYS: std::sync::LazyLock<tokio::sync::Mutex<CachedKeys>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));
async fn google_keys(state: &AppState, token: &str) -> Result<jsonwebtoken::jwk::JwkSet, ()> {
    let header = jsonwebtoken::decode_header(token).map_err(|_| ())?;
    let kid = header.kid.ok_or(())?;
    let mut cache = GOOGLE_KEYS.lock().await;
    if let Some((at, keys)) = cache.as_ref() {
        // Refresh on rotation, but bound refresh frequency for unknown key IDs.
        if at.elapsed().as_secs() < 300
            && (keys.find(&kid).is_some() || at.elapsed().as_secs() < 30)
        {
            return Ok(keys.clone());
        }
    }
    let keys: jsonwebtoken::jwk::JwkSet = state
        .http_client
        .get("https://www.googleapis.com/oauth2/v3/certs")
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())?;
    *cache = Some((std::time::Instant::now(), keys.clone()));
    Ok(keys)
}
#[derive(Deserialize)]
pub struct LoginExchange {
    code: String,
    verifier: String,
}
pub async fn kakao_exchange(
    State(state): State<AppState>,
    Json(q): Json<LoginExchange>,
) -> Result<Json<Value>, AuthError> {
    exchange(&state, q, "kakao").await
}
pub async fn google_exchange(
    State(state): State<AppState>,
    Json(q): Json<LoginExchange>,
) -> Result<Json<Value>, AuthError> {
    exchange(&state, q, "google").await
}
async fn exchange(
    state: &AppState,
    q: LoginExchange,
    provider: &str,
) -> Result<Json<Value>, AuthError> {
    if q.code.len() != 64 || q.verifier.len() != 64 {
        return Err(auth_error(StatusCode::BAD_REQUEST, "Invalid exchange"));
    }
    let row = sqlx::query_as::<_,(Uuid,bool)>("DELETE FROM oauth_handoffs WHERE exchange_hash=$1 AND challenge=$2 AND provider=$3 AND expires_at>now() AND callback_used AND user_id IS NOT NULL RETURNING user_id,is_new")
        .bind(handoff_hash(&q.code)).bind(handoff_hash(&q.verifier)).bind(provider)
        .fetch_optional(&state.pool).await.map_err(crate::errors::internal_error)?
        .ok_or_else(|| auth_error(StatusCode::UNAUTHORIZED, "로그인이 만료되었거나 이미 사용됐어요."))?;
    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id=$1")
        .bind(row.0)
        .fetch_one(&state.pool)
        .await
        .map_err(crate::errors::internal_error)?;
    let token = create_token(row.0, &role, &state.jwt_secret)
        .map_err(|_| auth_error(StatusCode::INTERNAL_SERVER_ERROR, "인증 오류"))?;
    let refresh = issue_refresh_token(&state.pool, row.0, &role, &state.jwt_secret)
        .await
        .map_err(|_| auth_error(StatusCode::INTERNAL_SERVER_ERROR, "인증 오류"))?;
    Ok(Json(
        json!({"token":token,"refresh_token":refresh,"is_new":row.1}),
    ))
}

#[cfg(test)]
mod oauth_tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    #[test]
    fn google_identity_requires_valid_signature_audience_issuer_expiry_and_nonce() {
        // Deliberately public fixture; NEVER use this key outside these tests.
        let key = EncodingKey::from_rsa_pem(include_bytes!(
            "../../tests/fixtures/oauth-test-only.private"
        ))
        .unwrap();
        let keys = serde_json::from_str(include_str!(
            "../../tests/fixtures/oauth-test-only.jwks.json"
        ))
        .unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("mazimi-test-only".into());
        let claims = json!({"sub":"google-123", "aud":"client-id", "iss":"https://accounts.google.com", "exp":chrono::Utc::now().timestamp()+300,"nonce":"test-nonce","name":"테스트"});
        let sign = |claims: &Value| encode(&header, claims, &key).unwrap();
        assert_eq!(
            verify_google_token(&sign(&claims), &keys, "client-id", "test-nonce")
                .unwrap()
                .sub,
            "google-123"
        );
        for (field, wrong) in [
            ("aud", json!("another-client")),
            ("iss", json!("https://attacker.invalid")),
            ("exp", json!(1)),
            ("nonce", json!("another-flow")),
            ("sub", json!("")),
            ("azp", json!("another-client")),
        ] {
            let mut modified = claims.clone();
            modified[field] = wrong;
            assert!(
                verify_google_token(&sign(&modified), &keys, "client-id", "test-nonce").is_err(),
                "accepted invalid {field}"
            );
        }
        let mut missing = claims.clone();
        missing.as_object_mut().unwrap().remove("nonce");
        assert!(verify_google_token(&sign(&missing), &keys, "client-id", "test-nonce").is_err());
        let token = sign(&claims);
        let mut parts: Vec<_> = token.split('.').map(str::to_owned).collect();
        let replacement = if parts[2].starts_with('A') { "B" } else { "A" };
        parts[2].replace_range(0..1, replacement);
        assert!(verify_google_token(&parts.join("."), &keys, "client-id", "test-nonce").is_err());
        header.kid = Some("unknown".into());
        assert!(verify_google_token(
            &encode(&header, &claims, &key).unwrap(),
            &keys,
            "client-id",
            "test-nonce"
        )
        .is_err());
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"public-key-is-not-a-secret"),
        )
        .unwrap();
        assert!(verify_google_token(&token, &keys, "client-id", "test-nonce").is_err());
    }
}
