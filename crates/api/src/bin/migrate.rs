use mazimi_core::{connect_database, DatabaseConfig, PoolRole};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = DatabaseConfig::from_env(PoolRole::Migration)?;
    let pool = connect_database(&config).await?;

    api::MIGRATOR.run(&pool).await?;
    pool.close().await;
    tracing::info!("database migrations applied successfully");
    Ok(())
}
