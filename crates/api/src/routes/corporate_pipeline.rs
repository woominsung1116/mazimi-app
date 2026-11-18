//! Corporate-benefit automation surfaces (design §2.4, §8.3).
//!
//! - `GET /api/v1/admin/corporate-pipeline/health` (admin): the daily
//!   self-reported health rows, the latest census summary and how the
//!   visible offers fall into the 기업혜택 browsing chips.
//! - `POST /api/v1/publisher/opt-out` (public, no login, flag
//!   `PUBLISHER_OPTOUT_ENABLED`, default off): a publisher stops collection
//!   and/or display for one of our source domains or a URL prefix on one.
//!   The request only records a token; nothing changes until the requester
//!   serves it on the domain (proof of control, `corporate_auto::optout`).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use mazimi_worker::corporate_auto::optout::{self, Scope};

use crate::auth::AdminUser;

type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

#[derive(Deserialize)]
pub struct HealthQuery {
    days: Option<i64>,
}

pub async fn health(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<HealthQuery>,
) -> Result<Json<Value>, ApiError> {
    let days = query.days.unwrap_or(14).clamp(1, 90);
    let rows = sqlx::query_as::<_, (chrono::NaiveDate, Value)>(
        "SELECT day, metrics FROM corporate_pipeline_daily ORDER BY day DESC LIMIT $1",
    )
    .bind(days)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let census = sqlx::query_as::<_, (uuid::Uuid, chrono::DateTime<chrono::Utc>, Value)>(
        "SELECT id, finished_at, summary FROM corporate_census_runs
         WHERE status = 'completed' ORDER BY finished_at DESC LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let decisions = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT mode, outcome, count(*) FROM corporate_evidence_checks
         WHERE checked_at > now() - interval '24 hours' GROUP BY mode, outcome ORDER BY mode, outcome",
    )
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let browse = browse_categories(&pool).await?;
    Ok(Json(json!({
        "browse_categories": browse,
        "days": rows.into_iter().map(|(day, metrics)| json!({"day": day, "metrics": metrics})).collect::<Vec<_>>(),
        "latest_census": census.map(|(id, at, summary)| json!({"run_id": id, "finished_at": at, "summary": summary})),
        "evidence_last_24h": decisions.into_iter().map(|(mode, outcome, count)| json!({"mode": mode, "outcome": outcome, "count": count})).collect::<Vec<_>>(),
    })))
}

/// How the 기업혜택 rows a visitor sees fall into the browsing chips
/// ([`mazimi_core::corporate_categories`]), read from the stored column with
/// the list's own visibility (`provider=corporate`, anonymous youth_fit
/// gate). `unmatched` counts rows no keyword recognised: rows the collector
/// adds land through the `benefit_category` fallback or in 기타 without
/// anyone noticing, so they are counted here and the 기타 titles listed.
/// `pending` rows have no stored chip yet (the worker job fills them).
async fn browse_categories(pool: &PgPool) -> Result<Value, ApiError> {
    use crate::routes::programs::{bind_all, list_filter, Bind, ProgramQuery};
    let query = ProgramQuery {
        provider: Some("corporate".into()),
        ..ProgramQuery::default()
    };
    let filter = list_filter(
        &query,
        mazimi_core::youth_fit::GateProfile::default(),
        false,
    )?;
    let rows_sql = format!(
        "SELECT programs.browse_category, programs.browse_category_by, count(*)
         FROM current_programs programs {} GROUP BY 1, 2 ORDER BY 1, 2",
        filter.where_clause
    );
    let rows = bind_all!(
        sqlx::query_as::<_, (Option<String>, Option<String>, i64)>(&rows_sql),
        filter.binds
    )
    .fetch_all(pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let other_sql = format!(
        "SELECT programs.title FROM current_programs programs {}
           AND programs.browse_category_by = 'none'
         ORDER BY programs.title LIMIT 50",
        filter.where_clause
    );
    let other = bind_all!(sqlx::query_scalar::<_, String>(&other_sql), filter.binds)
        .fetch_all(pool)
        .await
        .map_err(crate::errors::internal_error)?;
    let by = |kind: Option<&str>| {
        rows.iter()
            .filter(|r| r.1.as_deref() == kind)
            .map(|r| r.2)
            .sum::<i64>()
    };
    Ok(json!({
        "visible": rows.iter().map(|r| r.2).sum::<i64>(),
        "decided_by_keyword": by(Some("keyword")),
        "decided_by_fallback": by(Some("fallback")),
        "unmatched": by(Some("fallback")) + by(Some("none")),
        "in_other": by(Some("none")),
        "pending": by(None),
        "other_titles": other,
        "by_category": rows.iter().filter(|r| r.0.is_some()).map(|(code, decided_by, count)| json!({"code": code, "decided_by": decided_by, "count": count})).collect::<Vec<_>>(),
    }))
}

/// `GET /api/v1/admin/publisher-optouts`: open opt-outs and those lifted in
/// the last 30 days (no tokens or contact hashes).
pub async fn optout_status(
    _admin: AdminUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, ApiError> {
    let rows = optout::status(&pool)
        .await
        .map_err(crate::errors::internal_error)?;
    Ok(Json(json!({ "optouts": rows })))
}

/// `POST /api/v1/admin/publisher-optouts/{id}/lift`: an operator lifts an
/// abusive or mistaken opt-out through the same reversible path; offers
/// come back only after re-verification.
pub async fn optout_lift(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, ApiError> {
    let lifted = optout::admin_lift(&pool, id, admin.0.id)
        .await
        .map_err(crate::errors::internal_error)?;
    if !lifted {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no open opt-out with this id"})),
        ));
    }
    tracing::info!(event = "publisher_optout.admin_lift", optout_id = %id, admin_id = %admin.0.id, "operator lifted a publisher opt-out");
    Ok(Json(json!({"status": "lifted", "id": id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptOutRequest {
    domain: Option<String>,
    url_prefix: Option<String>,
    scope: String,
    contact: Option<String>,
}

pub fn optout_enabled() -> bool {
    optout::intake_enabled()
}

/// Normalized opt-out: domain, URL prefix, scope, contact hash.
type ParsedOptOut = (Option<String>, Option<String>, Scope, Option<String>);

pub(crate) fn parse_optout(request: &OptOutRequest) -> Result<ParsedOptOut, &'static str> {
    let scope = Scope::parse(&request.scope).ok_or("scope must be collection, display or both")?;
    let domain = match request
        .domain
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(value) => Some(
            optout::registrable_host(value)
                .ok_or("domain must be a registrable hostname, not a public suffix")?,
        ),
        None => None,
    };
    let url_prefix = match request
        .url_prefix
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(value) => {
            let url = reqwest::Url::parse(value).map_err(|_| "url_prefix must be an https URL")?;
            if url.scheme() != "https"
                || value.chars().count() > 2_000
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some()
                || url.host_str().and_then(optout::registrable_host).is_none()
            {
                return Err("url_prefix must be an https URL on a registrable hostname");
            }
            Some(url.to_string())
        }
        None => None,
    };
    if domain.is_some() == url_prefix.is_some() {
        return Err("exactly one of domain or url_prefix is required");
    }
    let contact = match request
        .contact
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(value) if value.chars().count() <= 200 => Some(
            Sha256::digest(value.to_lowercase().as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        ),
        Some(_) => return Err("contact must be at most 200 characters"),
        None => None,
    };
    Ok((domain, url_prefix, scope, contact))
}

/// Records the request; it takes effect once domain control is proven (see
/// `corporate_auto::optout`).
pub async fn opt_out(
    State(pool): State<PgPool>,
    Json(request): Json<OptOutRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !optout_enabled() {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))));
    }
    let (domain, url_prefix, scope, contact_hash) = parse_optout(&request).map_err(bad_request)?;
    let submission = optout::Submission {
        domain,
        url_prefix,
        scope,
        contact_hash,
    };
    match optout::submit(&pool, &submission).await {
        Ok(submitted) => Ok((StatusCode::ACCEPTED, Json(submitted.response(scope)))),
        Err(optout::SubmitError::UnknownSource) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "the domain is not a source we collect from"})),
        )),
        Err(optout::SubmitError::TooManyForHost) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "too many requests for this domain"})),
        )),
        Err(optout::SubmitError::Paused) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({"error": "opt-out intake is temporarily paused; robots.txt changes are still honoured"}),
            ),
        )),
        Err(optout::SubmitError::Internal(error)) => Err(crate::errors::internal_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(domain: Option<&str>, url: Option<&str>, scope: &str) -> OptOutRequest {
        OptOutRequest {
            domain: domain.map(str::to_string),
            url_prefix: url.map(str::to_string),
            scope: scope.into(),
            contact: Some("Partner@Example.com".into()),
        }
    }

    #[test]
    fn optout_input_is_normalized_and_validated() {
        let (domain, prefix, scope, contact) =
            parse_optout(&request(Some("WWW.Example.co.kr."), None, "both")).unwrap();
        assert_eq!(domain.as_deref(), Some("example.co.kr"));
        assert!(prefix.is_none());
        assert_eq!(scope, Scope::Both);
        assert_eq!(contact.unwrap().len(), 64);
        assert!(parse_optout(&request(None, None, "both")).is_err());
        // Public suffixes, unknown suffixes and IPs are refused.
        for suffix in [
            "co.kr",
            "github.io",
            "com",
            "ac.kr",
            "example.invalidtld",
            "127.0.0.1",
        ] {
            assert!(
                parse_optout(&request(Some(suffix), None, "both")).is_err(),
                "{suffix}"
            );
        }
        assert!(parse_optout(&request(None, Some("https://co.kr/"), "both")).is_err());
        assert!(parse_optout(&request(
            None,
            Some("https://shop.example.com:8443/x"),
            "both"
        ))
        .is_err());
        assert!(parse_optout(&request(
            Some("example.com"),
            Some("https://example.com/x"),
            "both"
        ))
        .is_err());
        assert_eq!(
            parse_optout(&request(Some("shop.example.co.kr"), None, "display"))
                .unwrap()
                .0
                .as_deref(),
            Some("shop.example.co.kr")
        );
        assert!(parse_optout(&request(Some("example.com"), None, "everything")).is_err());
        assert!(parse_optout(&request(Some("exa mple.com"), None, "both")).is_err());
        assert!(parse_optout(&request(Some("localhost"), None, "both")).is_err());
        assert!(parse_optout(&request(None, Some("http://example.com/x"), "display")).is_err());
        assert!(parse_optout(&request(
            None,
            Some("https://example.com/benefit"),
            "display"
        ))
        .is_ok());
    }
}
