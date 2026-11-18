//! Corporate benefits without humans in the loop, phases P0 and P1
//! (design: `.omc/plans/corporate-autonomous.md`).
//!
//! - [`census`] (P0): a read-only policy dry run. For every source domain,
//!   candidate and monitor it records robots per path for our agent, a ToS
//!   signal and the lane a URL can be collected through. It never changes
//!   crawl permission or publication.
//! - [`evidence`] (P1): a published fact stays public while its verbatim
//!   quote is still on the live first-party page and the deterministic
//!   parser reads the same value. Otherwise the offer is hidden. P1 never
//!   changes a fact.
//! - [`resilience`]: redesign-tolerant evidence (normalized anchors,
//!   element relocation, proven value and URL changes).
//! - [`reextract`]: fresh extraction of sources the evidence job flagged
//!   (`reextract_requested_at`), written as proposals; [`publish`] restores
//!   one in live mode once every new-candidate gate passed.
//! - [`weekly_audit`]: once a week every corporate_official offer is read
//!   again with the fresh extractor and compared with its stored values
//!   (agrees / disagrees / unreadable). Measurement only: it never hides,
//!   publishes or changes a programme, in any mode.
//! - [`health`]: one self-reported metrics row per day plus WARN events.
//! - [`optout`]: publisher opt-out intake (reversible hold) and the
//!   `.well-known` proof that makes it permanent or lifts it.
//!
//! Every network request goes through [`http`] and [`policy`]: an honest
//! `MazimiBenefitBot` user agent with a contact URL, one request in flight
//! per domain, robots Crawl-delay with a 10 s floor, a stop on
//! 401/403/429/challenge, no redirects followed off the host, no proxy.
//! Every flag defaults to off.

pub mod bootstrap;
pub mod census;
pub mod doors;
pub mod events;
pub mod evidence;
pub mod health;
pub mod http;
pub mod intake;
pub mod optout;
pub mod policy;
pub mod publish;
pub mod reextract;
pub mod resilience;
pub mod reverify;
pub mod snapshot;
pub mod values;
pub mod weekly_audit;

#[cfg(test)]
mod compliance_replay_tests;

use anyhow::{bail, Result};

/// Absolute minimum spacing between two requests to one domain (A4).
pub const MIN_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// Spacing used when the per-domain `gray_crawl_delay_min` switch is off (G3).
pub const STRICT_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// A boolean flag that refuses any value but a known on or off word, so a
/// typo stops the worker instead of silently leaving the flag off.
pub(crate) fn strict_flag(name: &str) -> Result<bool> {
    parse_strict_flag(name, &std::env::var(name).unwrap_or_default())
}

fn parse_strict_flag(name: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => bail!("{name} must be true or false, got {other:?}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Decisions are computed and logged; nothing public changes.
    Shadow,
    /// Hides for the allowlisted reasons ([`Flags::hide_reasons`]) apply;
    /// nothing is ever made visible (no confirmation, restore, publish or
    /// URL move). It cannot show an offer that is not already shown.
    HideOnly,
    /// Decisions hide, restore and re-date offers.
    Live,
}

impl VerifyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::HideOnly => "hide_only",
            Self::Live => "live",
        }
    }
}

/// Hide reasons `hide_only` applies by default: the source URL gone
/// (404/410 confirmed an hour later, held 24 h). The section and structured
/// reasons (`section_ended_confirmed`, `structured_ended`, …) stay shadow
/// metrics until a real-page shadow measurement shows no false hide; an
/// operator may list them in `CORPORATE_AUTO_HIDE_REASONS`.
pub const DEFAULT_HIDE_REASONS: &[&str] = &["source_gone"];

/// Every reason key `CORPORATE_AUTO_HIDE_REASONS` may list.
fn hide_reason_keys() -> Vec<&'static str> {
    resilience::HIDE_REASONS
        .iter()
        .filter_map(|r| resilience::reason_key(r))
        .chain(["source_gone"])
        .collect()
}

/// The `hide_only` allowlist from `CORPORATE_AUTO_HIDE_REASONS` (comma
/// separated reason keys), [`DEFAULT_HIDE_REASONS`] when unset or empty.
pub fn hide_reasons_from_env() -> Result<Vec<String>> {
    let raw = std::env::var("CORPORATE_AUTO_HIDE_REASONS").unwrap_or_default();
    let listed: Vec<String> = raw
        .split(',')
        .map(|key| key.trim().to_ascii_lowercase())
        .filter(|key| !key.is_empty())
        .collect();
    if listed.is_empty() {
        return Ok(DEFAULT_HIDE_REASONS.iter().map(|k| k.to_string()).collect());
    }
    let known = hide_reason_keys();
    if let Some(unknown) = listed.iter().find(|key| !known.contains(&key.as_str())) {
        bail!("CORPORATE_AUTO_HIDE_REASONS has an unknown reason {unknown:?}; known: {known:?}");
    }
    Ok(listed)
}

/// Environment flags. Every one defaults to off; a global flag that is off
/// overrides every per-domain switch.
#[derive(Debug, Clone)]
pub struct Flags {
    pub census_enabled: bool,
    pub verify_enabled: bool,
    pub verify_mode: VerifyMode,
    /// `CORPORATE_AUTO_PUBLISH_ENABLED`: whether live may write new values
    /// (fresh-extraction publish, a moved page's URL). Off by default: live
    /// then applies the reader's confirm, restore and hide decisions only,
    /// and fresh extractions are recorded as proposals.
    pub publish_enabled: bool,
    /// `CORPORATE_WEEKLY_AUDIT_ENABLED`: the weekly re-extraction audit
    /// ([`weekly_audit`]). Measurement only in every mode.
    pub weekly_audit_enabled: bool,
    /// G1: domains whose ToS has only a generic automated-access clause.
    pub gray_generic_tos: bool,
    /// Public contact URL placed in the user agent. Required when any
    /// network feature is on; there is no default identity.
    pub contact_url: Option<String>,
}

impl Flags {
    pub fn from_env() -> Result<Self> {
        let census_enabled = flag("CORPORATE_CENSUS_ENABLED");
        let verify_enabled = flag("CORPORATE_AUTO_VERIFY_ENABLED");
        let verify_mode = match std::env::var("CORPORATE_AUTO_VERIFY_MODE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "shadow" => VerifyMode::Shadow,
            "hide_only" => VerifyMode::HideOnly,
            "live" => VerifyMode::Live,
            other => {
                bail!("CORPORATE_AUTO_VERIFY_MODE must be shadow, hide_only or live, got {other:?}")
            }
        };
        hide_reasons_from_env()?;
        let publish_enabled = strict_flag("CORPORATE_AUTO_PUBLISH_ENABLED")?;
        let weekly_audit_enabled = strict_flag(weekly_audit::ENABLED_FLAG)?;
        let contact_url = std::env::var("CORPORATE_BOT_CONTACT_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(url) = contact_url.as_deref() {
            validate_contact_url(url)?;
        }
        if (census_enabled || verify_enabled || weekly_audit_enabled) && contact_url.is_none() {
            bail!("CORPORATE_BOT_CONTACT_URL is required when the corporate census, auto-verify or weekly audit is enabled");
        }
        Ok(Self {
            census_enabled,
            verify_enabled,
            verify_mode,
            publish_enabled,
            weekly_audit_enabled,
            gray_generic_tos: flag("GRAY_GENERIC_TOS_ENABLED"),
            contact_url,
        })
    }

    pub fn user_agent(&self) -> Option<String> {
        self.contact_url.as_deref().map(user_agent)
    }
}

/// The only user agent this module ever sends.
pub fn user_agent(contact_url: &str) -> String {
    format!(
        "{}/1.0 (+{contact_url})",
        crate::sources::benefit_pages::BOT_PRODUCT_TOKEN
    )
}

pub(crate) fn validate_contact_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    if url.scheme() != "https" || url.host_str().is_none() || value.chars().count() > 200 {
        bail!("CORPORATE_BOT_CONTACT_URL must be a public https URL of at most 200 characters");
    }
    if value.contains(|c: char| c.is_whitespace() || c == ')' || c == '(') {
        bail!("CORPORATE_BOT_CONTACT_URL must not contain spaces or parentheses");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_honest_and_carries_the_contact() {
        assert_eq!(
            user_agent("https://mazimi.kr/crawler-policy"),
            "MazimiBenefitBot/1.0 (+https://mazimi.kr/crawler-policy)"
        );
    }

    #[test]
    fn the_publish_flag_is_off_by_default_and_refuses_unknown_values() {
        let parse = |raw: &str| parse_strict_flag("CORPORATE_AUTO_PUBLISH_ENABLED", raw);
        for off in ["", "0", "false", "off", " No "] {
            assert!(!parse(off).unwrap(), "{off:?}");
        }
        for on in ["1", "true", "ON", "yes"] {
            assert!(parse(on).unwrap(), "{on:?}");
        }
        for bad in ["enabled", "tru", "2"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_weekly_audit_flag_is_off_by_default_and_refuses_unknown_values() {
        let parse = |raw: &str| parse_strict_flag(weekly_audit::ENABLED_FLAG, raw);
        for off in ["", "0", "false", "off", " no "] {
            assert!(!parse(off).unwrap(), "{off:?}");
        }
        for on in ["1", "true", "On", "YES"] {
            assert!(parse(on).unwrap(), "{on:?}");
        }
        for bad in ["enabled", "weekly", "tru", "2", "shadow"] {
            let error = parse(bad).unwrap_err().to_string();
            assert!(error.contains("CORPORATE_WEEKLY_AUDIT_ENABLED"), "{error}");
        }
    }

    #[test]
    fn contact_url_must_be_https_without_spaces() {
        assert!(validate_contact_url("https://mazimi.kr/crawler-policy").is_ok());
        assert!(validate_contact_url("http://mazimi.kr/bot").is_err());
        assert!(validate_contact_url("https://mazimi.kr/a b").is_err());
        assert!(validate_contact_url("https://mazimi.kr/a)b").is_err());
    }
}
