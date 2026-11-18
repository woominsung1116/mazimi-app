//! What one re-verification of a live offer means, and what follows from it.
//!
//! Every evidence check lands in one of four classes ([`Class`]):
//!
//! - **confirmed**: the official page states the offer as stored;
//! - **refuted**: positive evidence that the offer ended, changed or may no
//!   longer be shown. The page says it ended, a value or a condition
//!   changed, the quote or the offer's section no longer reads as stored,
//!   the page is gone (a 404/410 hides at once and retires once read again
//!   24 h later; a re-fetch an hour on that reads the page again restores
//!   it), the page redirects off the site, the publisher opted out, or our
//!   own domain policy withdrew the source (`domain_withdrawn`). The only
//!   class that hides at once;
//! - **transient**: we could not read the page this time: a network error,
//!   5xx, 429, a timeout, a response we cannot decode;
//! - **blocked**: we may not read the page: robots disallows it, a bot wall
//!   or login stands in front of it, or the census does not rate it direct
//!   HTML.
//!
//! Transient and blocked say nothing about the offer, so neither hides it
//! while its last confirmation is younger than [`GRACE`]. Retries follow a
//! backoff with a per-source jitter ([`retry_delay`]) so a provider outage
//! is retried across runs, not hammered in one. Once the grace window has
//! passed with no confirmation, the offer is hidden with a reversible hold
//! and one operational incident per source is opened; both close by
//! themselves at the next confirmation. Nobody has to review anything.
//!
//! Why 72 hours ([`GRACE`]), from production evidence history
//! (2026-10-20..11-04, 592 confirmed programme-days):
//! - every transient episode that ended did so within 2.04 days (an
//!   IKEA fetch failure, an Apple 429 back-off), so 72 h with retries at
//!   3 h, 6 h, 12 h and 24 h covers them;
//! - no confirmed offer was followed by a value change or an ending in
//!   that history. By the rule of three the daily rate of a real change is
//!   at most 3/592 = 0.51 % (95 %), so an offer shown unread for 72 h is
//!   wrong with probability at most 1.5 %, inside the 2 % error budget of
//!   the 98 % confirmed-precision bar. A week would be 3.5 %, over it;
//! - offers with an amount in their value window and offers without one
//!   changed section at the same rate (3.3 % and 2.7 % of programme-days,
//!   all section rewrites, none a value change), so one window serves
//!   both; nothing in the data supports a per-type split.
//!
//! The 14-day review deadline ([`super::evidence::EVIDENCE_MAX_AGE_DAYS`])
//! stays the backstop when the job itself stops.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// How long after its last confirmation an offer we cannot read stays
/// shown. See the module docs for the data behind it.
pub const GRACE: Duration = Duration::hours(72);

/// A run whose share of unreadable decisions (transient or blocked) exceeds
/// this raises one operational alert; a run below it resolves the alert.
/// Counted per offer decision, so a source with several offers weighs by
/// what it holds back.
pub const ALERT_UNREADABLE_SHARE: f64 = 0.25;
/// A run with fewer decisions than this neither raises nor resolves the
/// alert: one failed decision of three is no outage.
pub const ALERT_MIN_CHECKS: usize = 4;

/// Holds an evidence confirmation never lifts: a publisher opt-out waits
/// for its own flow. One list for every restore that rests on a verbatim
/// re-read of the official page (the evidence pass and a published fresh
/// extraction). The hash path (`source_monitoring`) lifts less: an unchanged
/// hash proves no access, quote or end date.
pub const ONLY_OWN_FLOW_LIFTS: &[&str] = &["publisher_optout"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Confirmed,
    Refuted,
    Transient,
    Blocked,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Refuted => "refuted",
            Self::Transient => "transient",
            Self::Blocked => "blocked",
        }
    }

    /// Whether the offer may stay shown, within its grace window, although
    /// nothing confirmed it.
    pub fn waits(self) -> bool {
        matches!(self, Self::Transient | Self::Blocked)
    }
}

/// The labels under which an offer is confirmed (shown).
pub const CONFIRMING: &[&str] = &["confirmed", "confirmed_fuzzy", "relocated", "url_adopted"];

/// The class of an evidence outcome label: a programme's decision on a page
/// that was read, or a source's outcome when there was no page.
pub fn class_of(outcome: &str) -> Class {
    if CONFIRMING.contains(&outcome) {
        return Class::Confirmed;
    }
    match outcome {
        "fetch_failed" | "rate_limited" | "unsupported" => Class::Transient,
        // `policy_blocked`: the census does not rate the page direct HTML.
        // `adoption_expired`: our own adoption window closed, nothing was
        // decided about the offer.
        "robots_disallowed" | "blocked" | "policy_blocked" | "adoption_expired" => Class::Blocked,
        _ => Class::Refuted,
    }
}

/// The hold an offer gets when its grace window ran out unconfirmed. Both
/// are released by the next confirmation.
pub fn grace_hold(class: Class) -> &'static str {
    match class {
        Class::Blocked => "access_revoked",
        _ => "source_unreachable",
    }
}

/// Whether an offer last confirmed at `confirmed_at` is still inside its
/// grace window at `now`. Never confirmed: no grace (fail closed).
pub fn in_grace(confirmed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    confirmed_at.is_some_and(|at| now - at < GRACE)
}

/// What an unconfirmed check (`class` transient or blocked) does to one
/// offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Stays shown; its grace window ends then.
    Shown(DateTime<Utc>),
    /// Hidden with this hold: its window ran out, or it was not shown.
    Hide(&'static str),
}

/// Only a shown offer waits, and only inside its grace window; one already
/// hidden stays hidden.
pub fn wait(
    class: Class,
    shown: bool,
    confirmed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Wait {
    match confirmed_at {
        Some(at) if shown && in_grace(Some(at), now) => Wait::Shown(at + GRACE),
        _ => Wait::Hide(grace_hold(class)),
    }
}

/// When to look again after an unconfirmed check, before any cap to the
/// grace window's end. `failures`: consecutive unconfirmed checks, this one
/// included. A 429 is the provider asking us to slow down, so it starts at
/// a day; other transient failures start at 3 h and double to a day. A
/// block is looked at daily while the offer is in grace (robots.txt or one
/// page a day) and weekly once it is hidden. The jitter (up to a quarter
/// of the delay, whole hours, fixed per source and attempt) spreads the
/// sources of one provider across runs.
pub fn retry_delay(outcome: &str, failures: i32, hidden: bool, candidate_id: Uuid) -> Duration {
    let n = (failures.max(1) - 1).min(5) as u32;
    let hours: i64 = match class_of(outcome) {
        Class::Blocked if hidden => 24 * 7,
        Class::Blocked => 24,
        Class::Transient if outcome == "rate_limited" => (24i64 << n).min(24 * 4),
        Class::Transient if hidden => (24i64 << n).min(24 * 7),
        Class::Transient => (3i64 << n).min(24),
        Class::Confirmed | Class::Refuted => 24,
    };
    Duration::hours(hours + jitter_hours(candidate_id, failures, hours / 4))
}

fn jitter_hours(candidate_id: Uuid, failures: i32, max: i64) -> i64 {
    if max <= 0 {
        return 0;
    }
    let digest = Sha256::digest(format!("{candidate_id}:{failures}").as_bytes());
    i64::from(u16::from_be_bytes([digest[0], digest[1]])) % (max + 1)
}

/// One run's self-metrics, by programme decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunHealth {
    pub confirmed: usize,
    pub refuted: usize,
    pub transient: usize,
    pub blocked: usize,
    /// Unconfirmed (transient or blocked) and still shown inside grace.
    pub in_grace: usize,
    /// Hidden by this run because grace ran out.
    pub hidden_after_grace: usize,
    /// Held sources with several programmes, which no reread republishes
    /// ([`super::evidence::MULTI_OFFER_SOURCE`]).
    pub multi_offer_held: usize,
    /// Held sources that rereading republished too often to queue again
    /// ([`super::evidence::VOLATILE_PAGE`]).
    pub volatile_held: usize,
}

impl RunHealth {
    pub fn count(&mut self, class: Class) {
        match class {
            Class::Confirmed => self.confirmed += 1,
            Class::Refuted => self.refuted += 1,
            Class::Transient => self.transient += 1,
            Class::Blocked => self.blocked += 1,
        }
    }

    pub fn checks(&self) -> usize {
        self.confirmed + self.refuted + self.transient + self.blocked
    }

    /// Share of checks we could not read (transient or blocked).
    pub fn unreadable_share(&self) -> f64 {
        match self.checks() {
            0 => 0.0,
            n => (self.transient + self.blocked) as f64 / n as f64,
        }
    }

    /// `Some(true)`: raise the alert; `Some(false)`: resolve it; `None`: too
    /// few checks to say.
    pub fn alert(&self) -> Option<bool> {
        (self.checks() >= ALERT_MIN_CHECKS)
            .then(|| self.unreadable_share() > ALERT_UNREADABLE_SHARE)
    }
}

#[cfg(test)]
#[path = "reverify_tests.rs"]
mod tests;
