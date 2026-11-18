//! `official-page-mcp`: the isolated live official-page fetch service.
//! No database, no user data; see the library docs.
use mazimi_official_page::{app, ServiceConfig};
use mazimi_worker::sources::live_page::{Limits, LiveFetcher, PinnedConnector};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Container healthcheck: the image has no curl or shell tools to rely on.
    if std::env::args().nth(1).as_deref() == Some("--health") {
        let bind = std::env::var("OFFICIAL_PAGE_BIND").unwrap_or_else(|_| "0.0.0.0:8090".into());
        let port = bind.rsplit(':').next().unwrap_or("8090");
        let ok = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        std::process::exit(if ok { 0 } else { 1 });
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    let token = std::env::var("OFFICIAL_PAGE_MCP_TOKEN").unwrap_or_default();
    anyhow::ensure!(
        token.len() >= 32,
        "OFFICIAL_PAGE_MCP_TOKEN must be set (32+ characters)"
    );
    let bind = std::env::var("OFFICIAL_PAGE_BIND").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let allowed_hosts: Vec<String> = std::env::var("OFFICIAL_PAGE_ALLOWED_HOSTS")
        .unwrap_or_else(|_| "official-page,official-page:8090".into())
        .split(',')
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .collect();
    let fetcher = Arc::new(LiveFetcher::new(
        Arc::new(PinnedConnector),
        Limits::default(),
    ));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "official page MCP service listening");
    axum::serve(
        listener,
        app(
            fetcher,
            ServiceConfig {
                allowed_hosts,
                token,
            },
        ),
    )
    .with_graceful_shutdown(async {
        // `docker stop` sends SIGTERM; a terminal sends SIGINT.
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    })
    .await?;
    Ok(())
}
