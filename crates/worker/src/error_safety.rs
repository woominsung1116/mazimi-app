//! Error text that is safe and bounded for durable operational storage.

const MAX_PERSISTED_ERROR_CHARS: usize = 1_000;
const SECRET_ENV_KEYS: &[&str] = &[
    "GOV_API_KEY",
    "GOV_BENEFITS_API_KEY",
    "YOUTH_CENTER_API_KEY",
    "KOSAF_API_KEY",
    "FSS_API_KEY",
    "NATIONAL_WELFARE_API_KEY",
    "LOCAL_WELFARE_API_KEY",
    "FTC_BRAND_API_KEY",
    "GEMINI_API_KEY",
    "KAKAO_BIZ_API_KEY",
    "KAKAO_BIZ_SENDER_KEY",
    "OPS_ALERT_WEBHOOK_URL",
    "DATABASE_URL",
    "DIRECT_DATABASE_URL",
    "JWT_SECRET",
    "SUPABASE_SERVICE_ROLE_KEY",
];

pub fn persisted_error(error: impl std::fmt::Display) -> String {
    let mut message = redact_url_credentials(&redact_url_queries(&error.to_string()));
    for key in SECRET_ENV_KEYS {
        if let Ok(secret) = std::env::var(key) {
            if secret.len() >= 8 {
                message = message.replace(&secret, "[REDACTED]");
            }
        }
    }

    let mut chars = message.chars();
    let bounded: String = chars.by_ref().take(MAX_PERSISTED_ERROR_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn redact_url_queries(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            if token.contains("://") {
                if let Some(query_start) = token.find('?') {
                    return format!("{}?[REDACTED]", &token[..query_start]);
                }
            }
            token.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_url_credentials(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            let Some(scheme_end) = token.find("://") else {
                return token.to_string();
            };
            let authority_start = scheme_end + 3;
            let authority_end = token[authority_start..]
                .find(['/', '?', '#'])
                .map(|n| authority_start + n)
                .unwrap_or(token.len());
            if let Some(at) = token[authority_start..authority_end].rfind('@') {
                format!(
                    "{}[REDACTED]@{}",
                    &token[..authority_start],
                    &token[authority_start + at + 1..]
                )
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::persisted_error;

    #[test]
    fn redacts_url_query_parameters() {
        let value = persisted_error(
            "request failed for https://example.com/path?serviceKey=super-secret&page=1",
        );
        assert_eq!(
            value,
            "request failed for https://example.com/path?[REDACTED]"
        );
        assert!(!value.contains("super-secret"));
    }

    #[test]
    fn redacts_database_credentials_and_query_tokens_together() {
        let error = persisted_error(
            "connection failed postgres://user:secret@db.example/db?password=another-secret",
        );
        assert_eq!(
            error,
            "connection failed postgres://[REDACTED]@db.example/db?[REDACTED]"
        );
    }

    #[test]
    fn bounds_persisted_error_size_on_character_boundary() {
        let value = persisted_error("가".repeat(1_500));
        assert_eq!(value.chars().count(), 1_001);
        assert!(value.ends_with('…'));
    }
}
