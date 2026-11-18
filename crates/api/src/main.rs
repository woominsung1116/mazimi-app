use anyhow::Context;
use mazimi_core::{connect_database, AppConfig};
use std::future::IntoFuture;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;
    init_tracing(&config.app_env);

    let pool = connect_database(&config.database).await?;

    if run_migrations_on_startup(&config.app_env)? {
        api::MIGRATOR
            .run(&pool)
            .await
            .context("database migration failed during API startup")?;
        tracing::info!("database migrations applied during local API startup");
    }

    tracing::info!(
        service = "mazimi-api",
        environment = %config.app_env,
        schema_version = api::latest_migration_version(),
        "database connected"
    );

    let app = api::build_app_with_runtime(
        pool.clone(),
        config.jwt_secret.clone(),
        config.app_env.clone(),
        config.public_api_base_url.clone(),
    );

    let addr = format!("0.0.0.0:{}", config.app_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind API listener at {addr}"))?;
    tracing::info!(service = "mazimi-api", address = %addr, "server listening");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = shutdown_rx.await;
    })
    .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result.context("API server exited unexpectedly")?,
        () = shutdown_signal() => {
            tracing::info!(service = "mazimi-api", "shutdown signal received");
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(config.shutdown_grace_period, &mut server).await {
                Ok(result) => result.context("API server failed during graceful shutdown")?,
                Err(_) => tracing::warn!(
                    grace_secs = config.shutdown_grace_period.as_secs(),
                    "graceful shutdown deadline exceeded; closing remaining connections"
                ),
            }
        }
    }

    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    tracing::info!(service = "mazimi-api", "shutdown complete");

    Ok(())
}

fn run_migrations_on_startup(app_env: &str) -> anyhow::Result<bool> {
    let default = app_env == "local" || app_env == "test";
    match std::env::var("API_RUN_MIGRATIONS_ON_STARTUP") {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => anyhow::bail!("API_RUN_MIGRATIONS_ON_STARTUP must be true or false"),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn init_tracing(app_env: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if matches!(app_env, "staging" | "production") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to register Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                () = ctrl_c => {},
                _ = terminate.recv() => {},
            },
            Err(error) => {
                tracing::error!(error = %error, "failed to register SIGTERM handler");
                ctrl_c.await;
            }
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
