use crate::database::{DatabaseConfig, PoolRole};
use anyhow::Context;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_env: String,
    pub app_port: u16,
    pub database: DatabaseConfig,
    pub jwt_secret: String,
    pub public_api_base_url: String,
    pub shutdown_grace_period: Duration,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let app_env = std::env::var("APP_ENV")
            .unwrap_or_else(|_| "production".to_string())
            .trim()
            .to_ascii_lowercase();
        anyhow::ensure!(
            matches!(
                app_env.as_str(),
                "local" | "test" | "staging" | "production"
            ),
            "APP_ENV must be one of: local, test, staging, production"
        );
        let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is required")?;

        // Reject weak JWT secrets in non-local environments.
        // A 32-character minimum ensures at least 256 bits of entropy
        // when the secret is random, and blocks placeholder values like
        // "change-me-in-production" from reaching staging/prod.
        if app_env != "local" && jwt_secret.len() < 32 {
            anyhow::bail!(
                "JWT_SECRET must be at least 32 characters in non-local environments (current length: {})",
                jwt_secret.len()
            );
        }

        let app_port = std::env::var("APP_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .context("APP_PORT must be a valid TCP port")?;
        anyhow::ensure!(app_port > 0, "APP_PORT must be greater than zero");

        let public_api_base_url = std::env::var("PUBLIC_API_BASE_URL").unwrap_or_else(|_| {
            if matches!(app_env.as_str(), "local" | "test") {
                let callback_host =
                    std::env::var("CALLBACK_HOST").unwrap_or_else(|_| "localhost".to_string());
                format!("http://{}:{app_port}", callback_host.trim())
            } else {
                std::env::var("DOMAIN")
                    .map(|domain| format!("https://{}/api", domain.trim().trim_end_matches('/')))
                    .unwrap_or_default()
            }
        });
        validate_public_api_base_url(&public_api_base_url, &app_env)?;

        let shutdown_grace_secs = std::env::var("SHUTDOWN_GRACE_SECS")
            .unwrap_or_else(|_| "30".to_string())
            .parse::<u64>()
            .context("SHUTDOWN_GRACE_SECS must be an integer")?;
        anyhow::ensure!(
            (5..=120).contains(&shutdown_grace_secs),
            "SHUTDOWN_GRACE_SECS must be between 5 and 120"
        );

        Ok(Self {
            app_env,
            app_port,
            database: DatabaseConfig::from_env(PoolRole::Api)?,
            jwt_secret,
            public_api_base_url: public_api_base_url.trim_end_matches('/').to_string(),
            shutdown_grace_period: Duration::from_secs(shutdown_grace_secs),
        })
    }
}

fn validate_public_api_base_url(value: &str, app_env: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty(),
        "PUBLIC_API_BASE_URL is required outside local/test environments"
    );
    let parsed = url::Url::parse(value).context("PUBLIC_API_BASE_URL must be an absolute URL")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "PUBLIC_API_BASE_URL must use http or https"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "PUBLIC_API_BASE_URL must not contain credentials"
    );
    anyhow::ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "PUBLIC_API_BASE_URL must not contain a query or fragment"
    );
    if matches!(app_env, "staging" | "production") {
        anyhow::ensure!(
            parsed.scheme() == "https",
            "PUBLIC_API_BASE_URL must use https in staging/production"
        );
    }
    Ok(())
}

#[cfg(test)]
mod app_config_tests {
    use super::validate_public_api_base_url;

    #[test]
    fn production_api_base_requires_https() {
        let error = validate_public_api_base_url("http://mazimi.kr/api", "production")
            .unwrap_err()
            .to_string();
        assert!(error.contains("https"));
    }

    #[test]
    fn api_base_rejects_embedded_credentials_and_queries() {
        assert!(
            validate_public_api_base_url("https://user:secret@mazimi.kr/api", "production")
                .is_err()
        );
        assert!(
            validate_public_api_base_url("https://mazimi.kr/api?token=secret", "production")
                .is_err()
        );
    }
}

/// Worker-specific config for external data source API keys.
/// Each key falls back to GOV_API_KEY if its own var is not set.
#[derive(Debug, Clone)]
pub struct WorkerApiKeys {
    /// 정부24 공공서비스 혜택 API (odcloud)
    pub gov_benefits_api_key: String,
    /// 온통청년 Open API
    pub youth_center_api_key: String,
    /// 한국장학재단 장학금 API (odcloud)
    pub kosaf_api_key: String,
    /// 금융감독원 금융상품 비교공시 API
    /// Optional: empty string means the key is not configured.
    pub fss_api_key: String,
}

impl WorkerApiKeys {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        // GOV_API_KEY is the shared odcloud key used as fallback
        // for sources that do not have their own dedicated env var.
        let gov_key = std::env::var("GOV_API_KEY").unwrap_or_default();

        let gov_benefits_api_key =
            std::env::var("GOV_BENEFITS_API_KEY").unwrap_or_else(|_| gov_key.clone());

        let youth_center_api_key =
            std::env::var("YOUTH_CENTER_API_KEY").unwrap_or_else(|_| gov_key.clone());

        let kosaf_api_key = std::env::var("KOSAF_API_KEY").unwrap_or_else(|_| gov_key.clone());

        let fss_api_key = std::env::var("FSS_API_KEY").unwrap_or_default();

        Ok(Self {
            gov_benefits_api_key,
            youth_center_api_key,
            kosaf_api_key,
            fss_api_key,
        })
    }
}
