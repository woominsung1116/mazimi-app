use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "mazimi-api",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub async fn health_detail(State(pool): State<PgPool>) -> Json<Value> {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
        .is_ok();

    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "db": db_ok,
    }))
}

/// Readiness is intentionally stricter than liveness. It verifies both DB
/// access and that the running binary's exact schema version is installed.
/// Orchestrators must stop routing traffic when this returns 503, but should
/// not restart a live process solely because of a brief upstream DB outage.
pub async fn readiness(State(pool): State<PgPool>) -> (StatusCode, Json<Value>) {
    let expected_version = crate::latest_migration_version();
    let check = tokio::time::timeout(Duration::from_secs(3), async {
        let database_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&pool)
            .await
            .is_ok();
        if !database_ok {
            return (false, false);
        }

        let installed_version = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version), 0)::bigint \
             FROM _sqlx_migrations WHERE success = true",
        )
        .fetch_one(&pool)
        .await;

        (
            true,
            installed_version
                .map(|version| version == expected_version)
                .unwrap_or(false),
        )
    })
    .await;

    let (database_ok, schema_ok) = check.unwrap_or((false, false));
    let ready = database_ok && schema_ok;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ready { "ready" } else { "not_ready" },
            "db": database_ok,
            "schema": schema_ok
        })),
    )
}
