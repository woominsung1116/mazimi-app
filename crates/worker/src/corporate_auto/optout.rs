//! Publisher opt-out (design §2.4).
//!
//! - **Intake** ([`submit`]): only a registrable domain (never a public
//!   suffix) that is one of our source domains, or a URL prefix on one. The
//!   request is recorded with a one-time token and changes nothing yet: the
//!   form is public and unauthenticated, so an unproven request must not be
//!   able to hide one source after another.
//! - **Proof** ([`run_scheduled`], hourly for the first 7 days, then daily):
//!   once the requester serves the token at
//!   `https://<host>/.well-known/mazimi-optout.txt`, the opt-out takes effect
//!   ([`apply_effect`]): collection stops for the domain (`scope`
//!   collection/both), the domain's offers are held with `publisher_optout`
//!   and then retired as withdrawn (`scope` display/both), stored page text
//!   is cleared. `compliance_status` is never touched. Everything changed is
//!   recorded in `previous_state`. Serving the token at
//!   `/.well-known/mazimi-optin.txt` lifts the opt-out; lifted offers go back
//!   through normal re-verification and are never shown unverified.
//! - Every check that honours an opt-out (collection, evidence, display)
//!   counts only effective ones (`effective_at IS NOT NULL`).
//! - Unproven requests expire after [`TOKEN_TTL_DAYS`]; fresh ones are
//!   checked first.
//! - A per-host daily cap on pending requests pauses web intake for that
//!   host and raises an operator alert;
//!   robots.txt changes, HTTP 410 and the e-mail contact keep working.

use anyhow::{Context, Result};
use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use tracing::{info, warn};
use uuid::Uuid;

use super::http::{PageHttp, PinnedHttp};
use super::policy::{DomainSession, Fetched};
use super::Flags;

/// Pending (unproven) web-form opt-outs per proof host in 24 h before intake
/// pauses for that host. Per host, so a flood against one source cannot
/// pause the form for every other publisher.
pub const DAILY_CAP: i64 = 20;
/// Days an unproven request's token stays valid; after that the proof pass
/// stops checking it (the response note and /crawler-policy say so).
pub const TOKEN_TTL_DAYS: i32 = 30;
/// Web-form opt-outs per host per hour.
pub const HOURLY_PER_HOST: i64 = 5;
/// Opt-outs whose proof files are checked per run.
const PROOF_CHECKS_PER_RUN: i64 = 20;
const PROOF_MAX_BYTES: usize = 4 * 1024;
pub const OPTOUT_PROOF_PATH: &str = "/.well-known/mazimi-optout.txt";
pub const OPTIN_PROOF_PATH: &str = "/.well-known/mazimi-optin.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Collection,
    Display,
    Both,
}

impl Scope {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "collection" => Some(Self::Collection),
            "display" => Some(Self::Display),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Display => "display",
            Self::Both => "both",
        }
    }

    fn stops_collection(self) -> bool {
        matches!(self, Self::Collection | Self::Both)
    }

    fn hides_offers(self) -> bool {
        matches!(self, Self::Display | Self::Both)
    }
}

/// Lowercase hostname without a trailing dot or a leading `www.`, accepted
/// only when it is a registrable domain or below one under a known public
/// suffix (`co.kr`, `github.io` and bare TLDs are refused; so are IPs).
pub fn registrable_host(value: &str) -> Option<String> {
    let host = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let valid_syntax = (4..=253).contains(&host.len())
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !valid_syntax {
        return None;
    }
    let suffix = psl::suffix(host.as_bytes())?;
    if !suffix.is_known() || suffix.as_bytes() == host.as_bytes() {
        return None;
    }
    psl::domain(host.as_bytes())?;
    Some(host)
}

#[derive(Debug, Clone)]
pub struct Submission {
    /// From [`registrable_host`].
    pub domain: Option<String>,
    /// An https URL whose host passes [`registrable_host`].
    pub url_prefix: Option<String>,
    pub scope: Scope,
    pub contact_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Submitted {
    pub id: Uuid,
    pub token: String,
    pub proof_host: String,
}

impl Submitted {
    pub fn response(&self, scope: Scope) -> Value {
        json!({
            "status": "pending_proof",
            "id": self.id,
            "effective": false,
            "scope": scope.as_str(),
            "verification": {
                "token": self.token,
                "confirm_url": format!("https://{}{OPTOUT_PROOF_PATH}", self.proof_host),
                "lift_url": format!("https://{}{OPTIN_PROOF_PATH}", self.proof_host),
                "note": "Nothing changes until this token is served at confirm_url. We check it about every hour for 7 days, then daily; once seen, collection stops and the offers are withdrawn. The token expires after 30 days if it has not been served. Serving it at lift_url later undoes the opt-out. robots.txt rules for MazimiBenefitBot apply without this step. The token is shown only once."
            }
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("the domain is not a source we collect from")]
    UnknownSource,
    #[error("too many requests for this domain")]
    TooManyForHost,
    #[error("opt-out intake is paused")]
    Paused,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for SubmitError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fingerprint of the operator alert raised when the daily cap pauses intake.
pub const PAUSED_ALERT_FINGERPRINT: &str = "publisher_optout:intake_paused";

#[derive(Debug, FromRow)]
struct DomainState {
    id: Uuid,
    crawl_allowed: bool,
    collection_mode: String,
}

#[derive(Debug, FromRow)]
struct HeldMonitor {
    program_id: Uuid,
    candidate_id: Uuid,
    is_active: bool,
    program_status: String,
    open_hold_reason: Option<String>,
}

/// Records an opt-out request. Nothing is stopped or hidden until the
/// requester proves control of the domain (see the module docs).
pub async fn submit(pool: &PgPool, submission: &Submission) -> Result<Submitted, SubmitError> {
    let proof_host = match (&submission.domain, &submission.url_prefix) {
        (Some(domain), _) => domain.clone(),
        (None, Some(prefix)) => Url::parse(prefix)
            .ok()
            .and_then(|u| u.host_str().and_then(registrable_host))
            .ok_or(SubmitError::UnknownSource)?,
        (None, None) => return Err(SubmitError::UnknownSource),
    };
    let mut tx = pool.begin().await?;
    // Serialises the caps and the effect across concurrent requests.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('publisher_optout_intake'))")
        .execute(&mut *tx)
        .await?;
    let today: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM benefit_source_optouts
         WHERE channel = 'web_form' AND received_at > now() - interval '24 hours'
           AND effective_at IS NULL AND lifted_at IS NULL
           AND (domain = $1 OR starts_with(url_prefix, 'https://' || $1 || '/')
                OR starts_with(url_prefix, 'https://www.' || $1 || '/'))",
    )
    .bind(&proof_host)
    .fetch_one(&mut *tx)
    .await?;
    if today >= DAILY_CAP {
        drop(tx);
        warn!(
            event = "publisher_optout.paused",
            received_24h = today,
            host = %proof_host,
            "publisher opt-out intake paused for a host by the daily cap"
        );
        // A spike is either a burst of genuine requests or abuse of the
        // public form; an operator looks at it either way.
        if let Err(error) = crate::operations::record_event(
            pool,
            &crate::operations::EventCommand {
                fingerprint: PAUSED_ALERT_FINGERPRINT,
                severity: crate::operations::Severity::Warning,
                component: "corporate_auto",
                event_type: "publisher_optout_intake_paused",
                subject_type: None,
                subject_id: None,
                message: "Publisher opt-out intake hit its daily cap for one host and is paused there; review recent requests in the admin opt-out list",
                details: json!({"host": proof_host, "pending_24h": today, "daily_cap": DAILY_CAP}),
            },
        )
        .await
        {
            warn!(event = "publisher_optout.alert_failed", error = %error, "could not record the intake pause alert");
        }
        return Err(SubmitError::Paused);
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM benefit_source_optouts
         WHERE channel = 'web_form' AND received_at > now() - interval '1 hour'
           AND (domain = $1 OR starts_with(url_prefix, 'https://' || $1 || '/')
                OR starts_with(url_prefix, 'https://www.' || $1 || '/'))",
    )
    .bind(&proof_host)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= HOURLY_PER_HOST {
        return Err(SubmitError::TooManyForHost);
    }

    // Our own source rows: the host itself, its www form and, for a whole
    // domain, its subdomains. Hosts are [a-z0-9.-] only, so LIKE is literal.
    let domains = sqlx::query_as::<_, DomainState>(
        "SELECT id, crawl_allowed, collection_mode FROM benefit_source_domains
         WHERE domain = $1 OR domain = 'www.' || $1 OR ($2 AND domain LIKE '%.' || $1)
         ORDER BY id",
    )
    .bind(&proof_host)
    .bind(submission.domain.is_some())
    .fetch_all(&mut *tx)
    .await?;
    // A brand whose offers we show only through an issuer hub (security
    // review H5) has no source domain of ours: its own registry domain is
    // enough, proved by the same `.well-known` file. Such an opt-out covers
    // the brand's door offers (`doors::lifecycle::OPTED_OUT_SQL`).
    if domains.is_empty() {
        let registry_brand: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM brand_discovery_targets t
                            WHERE t.is_active AND t.official_domain IS NOT NULL
                              AND regexp_replace(lower(t.official_domain), '^www\\.', '')
                                  = regexp_replace($1, '^www\\.', ''))",
        )
        .bind(&proof_host)
        .fetch_one(&mut *tx)
        .await?;
        if !registry_brand || submission.domain.is_none() {
            return Err(SubmitError::UnknownSource);
        }
    }
    let domain_ids: Vec<Uuid> = domains.iter().map(|d| d.id).collect();
    let url_prefix = match &submission.url_prefix {
        Some(prefix) => Some(source_form(&mut tx, &domain_ids, prefix).await?),
        None => None,
    };
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_optouts
             (domain, url_prefix, scope, channel, contact_hash, domain_ids, verification_token_hash)
         VALUES ($1, $2, $3, 'web_form', $4, $5, $6) RETURNING id",
    )
    .bind(&submission.domain)
    .bind(&url_prefix)
    .bind(submission.scope.as_str())
    .bind(&submission.contact_hash)
    .bind(&domain_ids)
    .bind(sha256_hex(&token))
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    info!(event = "publisher_optout.received", optout_id = %id, scope = submission.scope.as_str(),
          "publisher opt-out recorded; waiting for proof of domain control");
    Ok(Submitted {
        id,
        token,
        proof_host,
    })
}

/// Domain control proven: applies the opt-out in one transaction and
/// returns the `previous_state` it recorded (what lifting restores).
async fn apply_effect(pool: &PgPool, item: &Pending) -> Result<Value> {
    let scope = Scope::parse(&item.scope).context("stored opt-out has an unknown scope")?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('publisher_optout_intake'))")
        .execute(&mut *tx)
        .await?;
    let domains = sqlx::query_as::<_, DomainState>(
        "SELECT id, crawl_allowed, collection_mode FROM benefit_source_domains
         WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(&item.domain_ids)
    .fetch_all(&mut *tx)
    .await?;
    let domain_ids: Vec<Uuid> = domains.iter().map(|d| d.id).collect();
    let url_prefix = item.url_prefix.clone();
    let id = item.id;

    // Programme rows first, before any hold, monitor or fact (review M4).
    let sources: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM benefit_source_candidates
         WHERE domain_id = ANY($1) AND ($2::text IS NULL OR starts_with(url, $2))",
    )
    .bind(&domain_ids)
    .bind(&url_prefix)
    .fetch_all(&mut *tx)
    .await?;
    lock_programmes(&mut tx, &sources).await?;

    // Collection: a whole-domain opt-out blocks the domain rows; a URL
    // prefix is enforced by the per-fetch opt-out check instead.
    let mut stopped = Vec::new();
    if scope.stops_collection() && item.domain.is_some() {
        for domain in &domains {
            stopped.push(
                json!({"id": domain.id, "crawl_allowed": domain.crawl_allowed,
                                "collection_mode": domain.collection_mode}),
            );
        }
        sqlx::query(
            "UPDATE benefit_source_domains
             SET crawl_allowed = false, collection_mode = 'blocked',
                 policy_notes = concat_ws(' ', nullif(policy_notes, ''), 'Publisher opt-out received; collection stopped.'),
                 updated_at = now()
             WHERE id = ANY($1)",
        )
        .bind(&domain_ids)
        .execute(&mut *tx)
        .await?;
    }

    // The offers sourced from those rows (matched through their monitors,
    // never through a programme's own official_url).
    let monitors = sqlx::query_as::<_, HeldMonitor>(
        "SELECT DISTINCT m.program_id, m.candidate_id, p.is_active, p.program_status,
                h.hold_reason AS open_hold_reason
         FROM program_source_monitors m
         JOIN benefit_source_candidates c ON c.id = m.candidate_id
         JOIN programs p ON p.id = m.program_id
         LEFT JOIN program_source_holds h
                ON h.program_id = m.program_id AND h.candidate_id = m.candidate_id AND h.resolved_at IS NULL
         WHERE c.domain_id = ANY($1)
           AND ($2::text IS NULL OR starts_with(c.url, $2))
           AND m.verification_state <> 'retired'
           AND p.program_type IN ('corporate_benefit', 'campus_benefit')
           AND p.retirement_reason IS NULL",
    )
    .bind(&domain_ids)
    .bind(&url_prefix)
    .fetch_all(&mut *tx)
    .await?;
    let program_ids: Vec<Uuid> = monitors.iter().map(|m| m.program_id).collect();
    let candidate_ids: Vec<Uuid> = monitors.iter().map(|m| m.candidate_id).collect();

    let mut holds = Vec::new();
    if scope.hides_offers() {
        for monitor in &monitors {
            holds.push(json!({
                "program_id": monitor.program_id, "candidate_id": monitor.candidate_id,
                "prior_hold_reason": monitor.open_hold_reason,
                "is_active": monitor.is_active, "program_status": monitor.program_status,
            }));
            // An open hold keeps the pre-hold state it recorded; only its
            // reason changes, so no other path can release it.
            sqlx::query(
                "INSERT INTO program_source_holds
                     (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                 SELECT p.id, $2, 'publisher_optout', p.is_active, p.program_status FROM programs p WHERE p.id = $1
                 ON CONFLICT (program_id, candidate_id) WHERE resolved_at IS NULL
                 DO UPDATE SET hold_reason = 'publisher_optout'",
            )
            .bind(monitor.program_id)
            .bind(monitor.candidate_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE program_source_monitors
                 SET verification_state = 'change_detected', change_detected_at = COALESCE(change_detected_at, now()),
                     last_error = 'publisher opt-out', updated_at = now()
                 WHERE program_id = $1 AND candidate_id = $2",
            )
            .bind(monitor.program_id)
            .bind(monitor.candidate_id)
            .execute(&mut *tx)
            .await?;
        }
        let mut distinct = candidate_ids.clone();
        distinct.sort();
        distinct.dedup();
        for candidate_id in distinct {
            crate::source_monitoring::refresh_program_health(&mut tx, candidate_id).await?;
        }
    }

    // Stored copies of the publisher's text go in every scope.
    sqlx::query("DELETE FROM corporate_offer_facts WHERE program_id = ANY($1)")
        .bind(&program_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE benefit_source_candidates c SET raw_text = NULL, extracted_payload = NULL, updated_at = now()
         WHERE c.domain_id = ANY($1) AND ($2::text IS NULL OR starts_with(c.url, $2))",
    )
    .bind(&domain_ids)
    .bind(&url_prefix)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE corporate_census_entries SET shadow = NULL
         WHERE program_id = ANY($1) OR candidate_id = ANY($2)
            OR ($3::text IS NULL AND domain_id = ANY($4))",
    )
    .bind(&program_ids)
    .bind(&candidate_ids)
    .bind(&url_prefix)
    .bind(&domain_ids)
    .execute(&mut *tx)
    .await?;

    let held = if scope.hides_offers() {
        let mut ids = program_ids.clone();
        ids.sort();
        ids.dedup();
        ids.len()
    } else {
        0
    };
    let previous_state = json!({"domains": stopped, "holds": holds});
    sqlx::query(
        "UPDATE benefit_source_optouts
         SET effective_at = now(), programs_withdrawn = $2, previous_state = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(held as i32)
    .bind(&previous_state)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(event = "publisher_optout.effective", optout_id = %item.id, scope = scope.as_str(),
          programs_hidden = held, domains_stopped = stopped.len(), "publisher opt-out applied after proof of domain control");
    Ok(previous_state)
}

/// Locks, in id order, every programme monitored from these sources: the
/// rows `refresh_program_health` and the hold/fact writes touch. The
/// proven effect, permanence, lift and `publish` all take this lock before writing any
/// hold, monitor or fact, so they cannot deadlock or interleave (review M4).
pub(crate) async fn lock_programmes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    candidate_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT p.id FROM programs p
         WHERE p.id IN (SELECT m.program_id FROM program_source_monitors m WHERE m.candidate_id = ANY($1))
         ORDER BY p.id FOR UPDATE",
    )
    .bind(candidate_ids)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A URL prefix in the host form our sources use: `https://www.x/…` and
/// `https://x/…` name the same pages, so when no source sits under the
/// given form but some sit under the other, the other is stored.
async fn source_form(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    domain_ids: &[Uuid],
    prefix: &str,
) -> Result<String, SubmitError> {
    let sources_under = "SELECT EXISTS (SELECT 1 FROM benefit_source_candidates
                                        WHERE domain_id = ANY($1) AND starts_with(url, $2))";
    let given: bool = sqlx::query_scalar(sources_under)
        .bind(domain_ids)
        .bind(prefix)
        .fetch_one(&mut **tx)
        .await?;
    if given {
        return Ok(prefix.to_string());
    }
    let other = match prefix.strip_prefix("https://www.") {
        Some(rest) => format!("https://{rest}"),
        None => prefix.replacen("https://", "https://www.", 1),
    };
    let alternative: bool = sqlx::query_scalar(sources_under)
        .bind(domain_ids)
        .bind(&other)
        .fetch_one(&mut **tx)
        .await?;
    Ok(if alternative {
        other
    } else {
        prefix.to_string()
    })
}

// ── proof of domain control ───────────────────────────────────────────────

#[derive(Debug, Clone, FromRow)]
struct Pending {
    id: Uuid,
    domain: Option<String>,
    url_prefix: Option<String>,
    scope: String,
    domain_ids: Vec<Uuid>,
    verification_token_hash: String,
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
    previous_state: Value,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProofSummary {
    pub checked: usize,
    pub verified: usize,
    pub lifted: usize,
    pub programs_retired: usize,
}

/// Where the proof files of one opt-out live: the host and its www form.
fn proof_hosts(pending: &Pending) -> Vec<String> {
    let host = pending.domain.clone().or_else(|| {
        pending
            .url_prefix
            .as_deref()
            .and_then(|p| Url::parse(p).ok())
            .and_then(|u| u.host_str().and_then(registrable_host))
    });
    host.map(|h| vec![h.clone(), format!("www.{h}")])
        .unwrap_or_default()
}

/// True when any proof host serves a 200 whose body names one of the
/// tokens (compared by hash, so the plain token is never stored).
async fn serves_token(
    session: &mut DomainSession<'_>,
    base: &(dyn Fn(&str) -> String + Sync),
    hosts: &[String],
    path: &str,
    token_hash: &str,
) -> bool {
    for host in hosts {
        let Ok(url) = Url::parse(&format!("{}{path}", base(host))) else {
            continue;
        };
        // The publisher asked for this one file to be read; a missing robots
        // file sets no rule for it.
        if let Fetched::Reply(reply) = session.text_file(&url, PROOF_MAX_BYTES, true).await {
            if reply.status == 200 {
                let body = String::from_utf8_lossy(&reply.body);
                let found = body
                    .split(|c: char| !c.is_ascii_hexdigit())
                    .filter(|word| word.len() == 64)
                    .any(|word| sha256_hex(&word.to_ascii_lowercase()) == token_hash);
                if found {
                    return true;
                }
            }
        }
        if session.stopped().is_some() {
            break;
        }
    }
    false
}

/// One pass over open opt-outs: lift on an opt-in proof, otherwise make an
/// unverified opt-out permanent on an opt-out proof. `base` maps a host to
/// its origin (`https://host` in production).
pub(crate) async fn check_proofs_with(
    pool: &PgPool,
    http: &dyn PageHttp,
    base: &(dyn Fn(&str) -> String + Sync),
    test_spacing: Option<std::time::Duration>,
) -> Result<ProofSummary> {
    let pending = sqlx::query_as::<_, Pending>(
        "SELECT id, domain, url_prefix, scope, domain_ids, verification_token_hash, verified_at,
                effective_at, previous_state
         FROM benefit_source_optouts
         WHERE lifted_at IS NULL AND verification_token_hash IS NOT NULL
           -- An unproven request's token expires; a proven one is still
           -- checked for an opt-in lift.
           AND (verified_at IS NOT NULL OR received_at > now() - make_interval(days => $2))
           AND (proof_checked_at IS NULL
                OR proof_checked_at < now() - CASE WHEN received_at > now() - interval '7 days'
                                                   THEN interval '1 hour' ELSE interval '1 day' END)
         -- Fresh requests first, so old rows cannot starve a new publisher.
         ORDER BY (received_at > now() - interval '7 days') DESC,
                  proof_checked_at NULLS FIRST, received_at
         LIMIT $1",
    )
    .bind(PROOF_CHECKS_PER_RUN)
    .bind(TOKEN_TTL_DAYS)
    .fetch_all(pool)
    .await
    .context("failed to load opt-outs awaiting proof")?;
    let mut summary = ProofSummary::default();
    for item in &pending {
        sqlx::query("UPDATE benefit_source_optouts SET proof_checked_at = now() WHERE id = $1")
            .bind(item.id)
            .execute(pool)
            .await?;
        let hosts = proof_hosts(item);
        let mut session = DomainSession::new(
            http,
            Some(pool),
            item.domain_ids.first().copied(),
            true,
            0,
            8,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        summary.checked += 1;
        if serves_token(
            &mut session,
            base,
            &hosts,
            OPTIN_PROOF_PATH,
            &item.verification_token_hash,
        )
        .await
        {
            lift(pool, item, &format!("well-known:{OPTIN_PROOF_PATH}")).await?;
            summary.lifted += 1;
            continue;
        }
        if item.verified_at.is_none()
            && serves_token(
                &mut session,
                base,
                &hosts,
                OPTOUT_PROOF_PATH,
                &item.verification_token_hash,
            )
            .await
        {
            let mut item = item.clone();
            if item.effective_at.is_none() {
                item.previous_state = apply_effect(pool, &item).await?;
            }
            summary.programs_retired += make_permanent(pool, &item).await?;
            summary.verified += 1;
        }
    }
    Ok(summary)
}

fn held_pairs(previous_state: &Value) -> Vec<(Uuid, Uuid, Value)> {
    previous_state["holds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hold| {
            let program = hold["program_id"].as_str()?.parse().ok()?;
            let candidate = hold["candidate_id"].as_str()?.parse().ok()?;
            Some((program, candidate, hold.clone()))
        })
        .collect()
}

/// Domain control proven: the held offers are retired as withdrawn by the
/// provider. Collection-only opt-outs retire nothing.
async fn make_permanent(pool: &PgPool, item: &Pending) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let mut retired = 0;
    let held = held_pairs(&item.previous_state);
    let candidates: Vec<Uuid> = held.iter().map(|(_, c, _)| *c).collect();
    lock_programmes(&mut tx, &candidates).await?;
    if Scope::parse(&item.scope).is_some_and(Scope::hides_offers) {
        for (program_id, candidate_id, _) in held {
            let changed = sqlx::query(
                "UPDATE programs SET is_active = false, program_status = 'closed', retired_at = COALESCE(retired_at, now()),
                     retirement_reason = 'provider_withdrawn', updated_at = now()
                 WHERE id = $1 AND retirement_reason IS NULL
                   AND EXISTS (SELECT 1 FROM program_source_holds h WHERE h.program_id = $1 AND h.candidate_id = $2
                               AND h.resolved_at IS NULL AND h.hold_reason = 'publisher_optout')",
            )
            .bind(program_id)
            .bind(candidate_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed == 0 {
                continue;
            }
            retired += 1;
            sqlx::query(
                "UPDATE program_source_monitors SET verification_state = 'retired',
                     last_error = 'publisher opt-out, domain control proven', updated_at = now()
                 WHERE program_id = $1",
            )
            .bind(program_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE program_source_holds SET resolved_at = now(), resolution = 'retired',
                     resolution_note = 'Publisher opt-out made permanent after proof of domain control'
                 WHERE program_id = $1 AND resolved_at IS NULL AND hold_reason = 'publisher_optout'",
            )
            .bind(program_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE programs SET source_health = 'retired' WHERE id = $1 AND source_health <> 'unmonitored'")
                .bind(program_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query(
        "UPDATE benefit_source_optouts SET verified_at = now(),
             retired_at = CASE WHEN $2 > 0 THEN now() END
         WHERE id = $1",
    )
    .bind(item.id)
    .bind(retired as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(event = "publisher_optout.verified", optout_id = %item.id, programs_retired = retired,
          "publisher opt-out proven and made permanent");
    Ok(retired)
}

/// Lifts an opt-out. Offers return to re-verification (hidden until the
/// normal path confirms them); a domain's collection settings come back
/// unless another active opt-out still covers it.
async fn lift(pool: &PgPool, item: &Pending, proof: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE benefit_source_optouts SET lifted_at = now(), lift_proof = $2 WHERE id = $1",
    )
    .bind(item.id)
    .bind(proof)
    .execute(&mut *tx)
    .await?;
    for domain in item.previous_state["domains"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(domain_id) = domain["id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) else {
            continue;
        };
        let still_covered: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM benefit_source_optouts
                            WHERE lifted_at IS NULL AND effective_at IS NOT NULL
                              AND scope IN ('collection', 'both')
                              AND url_prefix IS NULL AND $1 = ANY(domain_ids))",
        )
        .bind(domain_id)
        .fetch_one(&mut *tx)
        .await?;
        if still_covered {
            continue;
        }
        let mode = domain["collection_mode"]
            .as_str()
            .unwrap_or("manual_review");
        let crawl = domain["crawl_allowed"].as_bool().unwrap_or(false);
        // The approval constraint may no longer hold (approval expired,
        // robots changed); then collection stays off and the normal
        // approval path decides.
        sqlx::query("SAVEPOINT optout_lift_domain")
            .execute(&mut *tx)
            .await?;
        let restored = sqlx::query(
            "UPDATE benefit_source_domains SET collection_mode = $2, crawl_allowed = $3, updated_at = now()
             WHERE id = $1 AND collection_mode = 'blocked'",
        )
        .bind(domain_id)
        .bind(mode)
        .bind(crawl)
        .execute(&mut *tx)
        .await;
        match restored {
            Ok(_) => {
                sqlx::query("RELEASE SAVEPOINT optout_lift_domain")
                    .execute(&mut *tx)
                    .await?;
            }
            Err(_) => {
                sqlx::query("ROLLBACK TO SAVEPOINT optout_lift_domain")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "UPDATE benefit_source_domains
                     SET collection_mode = CASE WHEN $2 IN ('direct_public', 'official_feed') THEN 'manual_review' ELSE $2 END,
                         crawl_allowed = false, updated_at = now()
                     WHERE id = $1 AND collection_mode = 'blocked'",
                )
                .bind(domain_id)
                .bind(mode)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    let held = held_pairs(&item.previous_state);
    let candidates: Vec<Uuid> = held.iter().map(|(_, c, _)| *c).collect();
    lock_programmes(&mut tx, &candidates).await?;
    for (program_id, candidate_id, hold) in held {
        let other_display_optout: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM benefit_source_optouts o
                            JOIN benefit_source_candidates c ON c.id = $1
                            WHERE o.lifted_at IS NULL AND o.effective_at IS NOT NULL
                              AND o.scope IN ('display', 'both')
                              AND c.domain_id = ANY(o.domain_ids)
                              AND (o.url_prefix IS NULL OR starts_with(c.url, o.url_prefix)))",
        )
        .bind(candidate_id)
        .fetch_one(&mut *tx)
        .await?;
        if other_display_optout {
            continue;
        }
        // The hold goes back to the reason it had before the opt-out took
        // it over (or waits for an ordinary re-verification), which restores
        // the pre-hold state only once the page is confirmed.
        let prior = hold["prior_hold_reason"]
            .as_str()
            .filter(|reason| *reason != "publisher_optout")
            .unwrap_or("verification_overdue");
        let retirement: Option<String> =
            sqlx::query_scalar("SELECT retirement_reason FROM programs WHERE id = $1")
                .bind(program_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        if retirement.is_some() {
            // Review M2: only a retirement this opt-out made is undone. Its
            // proof resolved the hold in the transaction that stamped the
            // opt-out verified, so the two times are equal. The resolved
            // hold also kept the state from before any hold (review M1).
            let ours: Option<(bool, String)> = sqlx::query_as(
                "SELECT h.previous_is_active, h.previous_program_status
                 FROM program_source_holds h JOIN benefit_source_optouts o ON o.id = $3
                 WHERE h.program_id = $1 AND h.candidate_id = $2 AND h.hold_reason = 'publisher_optout'
                   AND h.resolution = 'retired' AND h.resolved_at = o.verified_at
                 ORDER BY h.resolved_at DESC LIMIT 1",
            )
            .bind(program_id)
            .bind(candidate_id)
            .bind(item.id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((previous_is_active, previous_status)) =
                ours.filter(|_| retirement.as_deref() == Some("provider_withdrawn"))
            else {
                continue;
            };
            sqlx::query(
                "UPDATE programs SET retirement_reason = NULL, retired_at = NULL, is_active = false,
                     program_status = 'review_required', updated_at = now()
                 WHERE id = $1",
            )
            .bind(program_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO program_source_holds
                     (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (program_id, candidate_id) WHERE resolved_at IS NULL DO NOTHING",
            )
            .bind(program_id)
            .bind(candidate_id)
            .bind(prior)
            .bind(previous_is_active)
            .bind(previous_status)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "UPDATE program_source_holds SET hold_reason = $3
                 WHERE program_id = $1 AND candidate_id = $2 AND resolved_at IS NULL AND hold_reason = 'publisher_optout'",
            )
            .bind(program_id)
            .bind(candidate_id)
            .bind(prior)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE program_source_monitors
             SET verification_state = 'review_due', review_due_at = now(), last_error = NULL, updated_at = now()
             WHERE program_id = $1 AND candidate_id = $2",
        )
        .bind(program_id)
        .bind(candidate_id)
        .execute(&mut *tx)
        .await?;
        crate::source_monitoring::refresh_program_health(&mut tx, candidate_id).await?;
    }
    tx.commit().await?;
    info!(event = "publisher_optout.lifted", optout_id = %item.id, "publisher opt-out lifted after proof of domain control");
    Ok(())
}

/// Operator lift (admin API): the same reversible path as a proof-based
/// lift, recorded as `admin:<user>`. False when the opt-out does not exist
/// or is already lifted. Offers still return only through re-verification.
pub async fn admin_lift(pool: &PgPool, id: Uuid, admin_id: Uuid) -> Result<bool> {
    let pending = sqlx::query_as::<_, Pending>(
        "SELECT id, domain, url_prefix, scope, domain_ids,
                COALESCE(verification_token_hash, '') AS verification_token_hash,
                verified_at, effective_at, previous_state
         FROM benefit_source_optouts WHERE id = $1 AND lifted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(item) = pending else {
        return Ok(false);
    };
    lift(pool, &item, &format!("admin:{admin_id}")).await?;
    Ok(true)
}

/// Opt-outs for the operator status view: every open one and those lifted
/// in the last 30 days. Contact hashes and tokens are never returned.
pub async fn status(pool: &PgPool) -> Result<Vec<Value>> {
    let rows = sqlx::query_as::<_, StatusRow>(
        "SELECT id, domain, url_prefix, scope, channel, received_at, verified_at, retired_at,
                lifted_at, programs_withdrawn, lift_proof
         FROM benefit_source_optouts
         WHERE lifted_at IS NULL OR lifted_at > now() - interval '30 days'
         ORDER BY received_at DESC LIMIT 500",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let lifted_by = row.lift_proof.map(|proof| {
                if proof.starts_with("admin:") {
                    "operator"
                } else {
                    "publisher_proof"
                }
            });
            json!({"id": row.id, "domain": row.domain, "url_prefix": row.url_prefix,
                   "scope": row.scope, "channel": row.channel, "received_at": row.received_at,
                   "verified_at": row.verified_at, "retired_at": row.retired_at,
                   "lifted_at": row.lifted_at, "programs_hidden": row.programs_withdrawn,
                   "lifted_by": lifted_by})
        })
        .collect())
}

#[derive(Debug, FromRow)]
struct StatusRow {
    id: Uuid,
    domain: Option<String>,
    url_prefix: Option<String>,
    scope: String,
    channel: String,
    received_at: chrono::DateTime<chrono::Utc>,
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
    retired_at: Option<chrono::DateTime<chrono::Utc>>,
    lifted_at: Option<chrono::DateTime<chrono::Utc>>,
    programs_withdrawn: i32,
    lift_proof: Option<String>,
}

pub fn intake_enabled() -> bool {
    super::flag("PUBLISHER_OPTOUT_ENABLED")
}

/// Worker entry point (hourly). Runs while intake is enabled; without a
/// bot contact URL it fetches nothing, so nothing is retired or lifted.
pub async fn run_scheduled(pool: &PgPool) {
    if !intake_enabled() {
        return;
    }
    let user_agent = match Flags::from_env() {
        Ok(flags) => flags.user_agent(),
        Err(error) => {
            warn!(event = "publisher_optout.config_invalid", error = %error, "opt-out proof check misconfigured");
            return;
        }
    };
    let Some(user_agent) = user_agent else {
        warn!(
            event = "publisher_optout.proof_skipped",
            "CORPORATE_BOT_CONTACT_URL is unset; opt-out proofs are not checked"
        );
        return;
    };
    let http = match PinnedHttp::new(user_agent, std::time::Duration::from_secs(20)) {
        Ok(http) => http,
        Err(error) => {
            warn!(event = "publisher_optout.config_invalid", error = %error, "opt-out proof transport refused");
            return;
        }
    };
    match check_proofs_with(pool, &http, &|host| format!("https://{host}"), None).await {
        Ok(summary) => info!(
            event = "publisher_optout.proofs",
            checked = summary.checked,
            verified = summary.verified,
            lifted = summary.lifted,
            programs_retired = summary.programs_retired,
            "opt-out proofs checked"
        ),
        Err(error) => {
            warn!(event = "publisher_optout.proofs_failed", error = %error, "opt-out proof check failed")
        }
    }
}

#[cfg(test)]
#[path = "optout_tests.rs"]
mod tests;
