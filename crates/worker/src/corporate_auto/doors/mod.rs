//! Corporate doors: the routes the server reads must-have brands through
//! (`.omc/plans/corporate-doors-impl-plan.md` as overridden by its review
//! `corporate-doors-plan-review-20261111.md`).
//!
//! - [`registry`]: must-have brands, their names and the issuers' holder
//!   keys; exact-element binding.
//! - [`rows`]: the hub row reader (the one hub lane) and list-row links.
//! - [`job`]: the daily door job: reads each due door through the polite
//!   [`super::policy::DomainSession`], records what it found
//!   (`corporate_door_reads`, `corporate_door_offers`), keeps door health
//!   and, for a live hub, publishes and hides its offers.
//! - [`lifecycle`]: lost doors, rediscovery from the host's sitemaps and
//!   replacement by brand overlap.
//! - [`metrics`]: the shadow gate numbers (review H7).
//!
//! **Switches, all off by default** (pinned by tests below):
//! - `CORPORATE_DOORS_MODE` = `off` | `shadow` | `live`. Off: the job
//!   returns without a query or a request. Shadow: doors are read and
//!   recorded, nothing is published. Live: kinds in
//!   `CORPORATE_DOOR_KINDS_LIVE` publish, and only while
//!   `CORPORATE_INTAKE_MODE` is `live` (the one kill switch of every
//!   automatic publication).
//! - `CORPORATE_DOOR_KINDS_LIVE`: a comma list of `page`, `listing`,
//!   `hub_rows`; empty by default.
//! - `CORPORATE_DOOR_FLAG_PAYCO`: a door carrying `flag = 'payco'` is read
//!   only while this is on.
//! - A `hub_rows` door is read only while its domain's
//!   `terms_review_outcome` is `explicit_permission` or
//!   `no_automated_access_restriction`, recorded by a person (review C3).
//!   With no outcome a hub is never read, in any mode.

pub mod job;
pub mod lifecycle;
pub mod metrics;
pub mod registry;
pub mod rows;

use anyhow::{bail, Result};
use std::collections::BTreeSet;

pub const MODE_FLAG: &str = "CORPORATE_DOORS_MODE";
pub const KINDS_LIVE_FLAG: &str = "CORPORATE_DOOR_KINDS_LIVE";
/// `CORPORATE_DOOR_FLAG_PAYCO` turns on the doors carrying `flag = 'payco'`.
pub const PAYCO_FLAG: &str = "CORPORATE_DOOR_FLAG_PAYCO";
/// Door flags a migration may set (`corporate_doors.flag` CHECK) and the
/// switch of each.
pub const DOOR_FLAGS: &[(&str, &str)] = &[("payco", PAYCO_FLAG)];

/// The terms outcomes under which a hub's rows may be read.
pub const HUB_TERMS_ALLOWED_SQL: &str =
    "COALESCE(d.terms_review_outcome, '') IN ('explicit_permission', 'no_automated_access_restriction')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorsMode {
    Off,
    Shadow,
    Live,
}

impl DoorsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Live => "live",
        }
    }
}

pub fn parse_mode(raw: &str) -> Result<DoorsMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "off" => Ok(DoorsMode::Off),
        "shadow" => Ok(DoorsMode::Shadow),
        "live" => Ok(DoorsMode::Live),
        other => bail!("{MODE_FLAG} must be off, shadow or live, got {other:?}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Page,
    Listing,
    HubRows,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Listing => "listing",
            Self::HubRows => "hub_rows",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "page" => Some(Self::Page),
            "listing" => Some(Self::Listing),
            "hub_rows" => Some(Self::HubRows),
            _ => None,
        }
    }
}

pub fn parse_live_kinds(raw: &str) -> Result<BTreeSet<Kind>> {
    raw.split(',')
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| !k.is_empty())
        .map(|k| match Kind::parse(&k) {
            Some(kind) => Ok(kind),
            None => bail!("{KINDS_LIVE_FLAG} lists an unknown door kind {k:?}"),
        })
        .collect()
}

/// The door switches, read once per run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorFlags {
    pub mode: DoorsMode,
    pub live_kinds: BTreeSet<Kind>,
    /// Door flags switched on (`CORPORATE_DOOR_FLAG_<FLAG>`).
    pub flags_on: Vec<String>,
    /// `CORPORATE_INTAKE_MODE` is `live`.
    pub intake_live: bool,
}

impl DoorFlags {
    pub fn from_env() -> Result<Self> {
        let mut flags_on = Vec::new();
        if super::strict_flag(PAYCO_FLAG)? {
            flags_on.push("payco".to_string());
        }
        debug_assert_eq!(DOOR_FLAGS.len(), 1, "read every door flag above");
        Ok(Self {
            mode: parse_mode(&std::env::var(MODE_FLAG).unwrap_or_default())?,
            live_kinds: parse_live_kinds(&std::env::var(KINDS_LIVE_FLAG).unwrap_or_default())?,
            flags_on,
            // The effective intake mode (security review M4): intake live and
            // the evidence job live, the same kill switch intake obeys.
            intake_live: super::intake::effective_mode(
                super::intake::parse_mode(
                    &std::env::var(super::intake::MODE_FLAG).unwrap_or_default(),
                )?,
                super::Flags::from_env().map_or(super::VerifyMode::Shadow, |f| f.verify_mode),
            ) == super::intake::Mode::Live,
        })
    }

    /// Whether the job reads at all.
    pub fn reads(&self) -> bool {
        self.mode != DoorsMode::Off
    }

    /// Whether offers of `kind` may become public.
    pub fn publishes(&self, kind: Kind) -> bool {
        self.mode == DoorsMode::Live && self.intake_live && self.live_kinds.contains(&kind)
    }
}

#[cfg(test)]
mod job_db_tests;
#[cfg(test)]
mod rows_tests;
#[cfg(test)]
mod scope_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every switch defaults off: deploying changes nothing.
    #[test]
    fn every_door_switch_defaults_off_and_refuses_unknown_values() {
        assert_eq!(parse_mode("").unwrap(), DoorsMode::Off);
        assert_eq!(parse_mode(" OFF ").unwrap(), DoorsMode::Off);
        assert_eq!(parse_mode("shadow").unwrap(), DoorsMode::Shadow);
        assert_eq!(parse_mode("live").unwrap(), DoorsMode::Live);
        for bad in ["on", "true", "1", "hide_only"] {
            assert!(parse_mode(bad).is_err(), "{bad}");
        }
        assert!(parse_live_kinds("").unwrap().is_empty());
        assert_eq!(
            parse_live_kinds("page, hub_rows").unwrap(),
            [Kind::Page, Kind::HubRows].into_iter().collect()
        );
        assert!(
            parse_live_kinds("app_lookup").is_err(),
            "the App Store lane is cut"
        );
        let off = DoorFlags {
            mode: DoorsMode::Off,
            live_kinds: BTreeSet::new(),
            flags_on: Vec::new(),
            intake_live: false,
        };
        assert!(!off.reads());
        for kind in [Kind::Page, Kind::Listing, Kind::HubRows] {
            assert!(!off.publishes(kind));
        }
        // Shadow reads and never publishes, whatever the kinds say.
        let shadow = DoorFlags {
            mode: DoorsMode::Shadow,
            live_kinds: [Kind::HubRows].into_iter().collect(),
            intake_live: true,
            ..off.clone()
        };
        assert!(shadow.reads() && !shadow.publishes(Kind::HubRows));
        // Live publishes only listed kinds and only under live intake.
        let live = DoorFlags {
            mode: DoorsMode::Live,
            live_kinds: [Kind::HubRows].into_iter().collect(),
            intake_live: false,
            ..off
        };
        assert!(
            !live.publishes(Kind::HubRows),
            "intake off is the kill switch"
        );
        let live = DoorFlags {
            intake_live: true,
            ..live
        };
        assert!(live.publishes(Kind::HubRows) && !live.publishes(Kind::Page));
    }

    /// With nothing set in the environment the flags read off.
    #[test]
    fn the_environment_default_is_off() {
        for name in [MODE_FLAG, KINDS_LIVE_FLAG, "CORPORATE_DOOR_FLAG_PAYCO"] {
            assert!(
                std::env::var(name).is_err(),
                "{name} must be unset in the test environment"
            );
        }
        let flags = DoorFlags::from_env().unwrap();
        assert_eq!(flags.mode, DoorsMode::Off);
        assert!(flags.live_kinds.is_empty() && flags.flags_on.is_empty());
    }
}
