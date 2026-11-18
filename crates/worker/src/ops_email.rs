//! SMTP delivery for operator alerts and the daily digest.
//!
//! Enabled by `OPS_ALERT_EMAIL_TO`. The password (`SMTP_PASSWORD`, for Gmail
//! an app password) is never logged: `EmailConfig` prints it redacted and
//! send errors are reduced to a fixed string before they are stored.

use anyhow::{Context, Result};
use lettre::{
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

#[derive(Clone)]
pub struct EmailConfig {
    pub to: Mailbox,
    pub from: Mailbox,
    pub host: String,
    pub port: u16,
    pub username: String,
    password: String,
}

impl std::fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailConfig")
            .field("to", &self.to.to_string())
            .field("from", &self.from.to_string())
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl EmailConfig {
    /// `None` when `OPS_ALERT_EMAIL_TO` is unset. A set recipient with an
    /// incomplete SMTP configuration is an error, so it is visible.
    pub fn from_env() -> Result<Option<Self>> {
        let Some(to) = env("OPS_ALERT_EMAIL_TO") else {
            return Ok(None);
        };
        let to: Mailbox = to.parse().context("OPS_ALERT_EMAIL_TO is not an address")?;
        let host = env("SMTP_HOST").unwrap_or_else(|| "smtp.gmail.com".to_string());
        let port: u16 = env("SMTP_PORT")
            .unwrap_or_else(|| "465".to_string())
            .parse()
            .context("SMTP_PORT must be 465 or 587")?;
        anyhow::ensure!(
            port == 465 || port == 587,
            "SMTP_PORT must be 465 (TLS) or 587 (STARTTLS)"
        );
        let username = env("SMTP_USERNAME").context("SMTP_USERNAME is required for email")?;
        // Gmail shows app passwords in groups of four ("abcd efgh ..."):
        // every whitespace character is dropped.
        let password = std::env::var("SMTP_PASSWORD")
            .ok()
            .map(|value| {
                value
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
            })
            .filter(|value| !value.is_empty())
            .context("SMTP_PASSWORD is required for email")?;
        let from: Mailbox = env("SMTP_FROM")
            .unwrap_or_else(|| username.clone())
            .parse()
            .context("SMTP_FROM is not an address")?;
        Ok(Some(Self {
            to,
            from,
            host,
            port,
            username,
            password,
        }))
    }

    fn transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
        let builder = if self.port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)
        }
        .context("invalid SMTP_HOST")?;
        Ok(builder
            .port(self.port)
            .credentials(Credentials::new(
                self.username.clone(),
                self.password.clone(),
            ))
            .timeout(Some(std::time::Duration::from_secs(20)))
            .build())
    }

    /// Sends one plain-text plus HTML message. Errors carry no server
    /// response text (it can echo credentials or addresses).
    pub async fn send(&self, subject: &str, text: &str, html: &str) -> Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(self.to.clone())
            .subject(subject)
            .multipart(MultiPart::alternative_plain_html(
                text.to_string(),
                html.to_string(),
            ))
            .context("email message could not be built")?;
        match self.transport()?.send(message).await {
            Ok(_) => Ok(()),
            Err(error) => {
                let kind = if error.is_permanent() {
                    "permanent"
                } else if error.is_transient() {
                    "transient"
                } else {
                    "connection"
                };
                anyhow::bail!("smtp send failed ({kind})")
            }
        }
    }
}

/// Minimal HTML escaping for operator text placed in the HTML body.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Wraps plain lines into a simple HTML document (one `<p>` per block, a
/// list for lines starting with "- ").
pub fn html_from_text(title: &str, text: &str) -> String {
    let mut body = format!("<h2>{}</h2>", escape_html(title));
    let mut in_list = false;
    for line in text.lines() {
        if let Some(item) = line.strip_prefix("- ") {
            if !in_list {
                body.push_str("<ul>");
                in_list = true;
            }
            body.push_str(&format!("<li>{}</li>", escape_html(item)));
            continue;
        }
        if in_list {
            body.push_str("</ul>");
            in_list = false;
        }
        if line.starts_with("■ ") {
            body.push_str(&format!("<h3>{}</h3>", escape_html(line)));
        } else if !line.trim().is_empty() {
            body.push_str(&format!("<p>{}</p>", escape_html(line)));
        }
    }
    if in_list {
        body.push_str("</ul>");
    }
    format!(
        "<!doctype html><html><body style=\"font-family:sans-serif;font-size:14px;line-height:1.5\">{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_body_escapes_operator_text() {
        let html = html_from_text("제목 <b>", "■ 섹션\n- a<script>\n문장 & 끝");
        assert!(html.contains("<h2>제목 &lt;b&gt;</h2>"));
        assert!(html.contains("<li>a&lt;script&gt;</li>"));
        assert!(html.contains("<p>문장 &amp; 끝</p>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let config = EmailConfig {
            to: "ops@example.org".parse().unwrap(),
            from: "bot@example.org".parse().unwrap(),
            host: "smtp.gmail.com".into(),
            port: 465,
            username: "bot@example.org".into(),
            password: "app-password-secret".into(),
        };
        let printed = format!("{config:?}");
        assert!(!printed.contains("app-password-secret"));
        assert!(printed.contains("<redacted>"));
    }
}
