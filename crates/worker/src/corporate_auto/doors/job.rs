//! The door job: one read of each due door, through the same polite
//! session every collector uses (robots per path with named-allowlist
//! refusal, `Visit-time`, ≥10 s per host, a stop on 401/403/429/challenge,
//! opt-out before every hop, no off-host redirect). Nothing here adds a way
//! around [`super::super::policy`].
//!
//! Per door kind ([`interpret`], pure):
//! - `hub_rows`: [`super::rows::read_hub`]; every accepted row is an offer
//!   of (grantor, redeem brand).
//! - `page`: [`bootstrap::extract_offer_scoped`] under the door brand's
//!   scope; the offer's issuer, when it names one, is its sponsor.
//!   A page found through a list (a child door) must also be dated
//!   ([`bootstrap::dated_article`], review M3).
//! - `listing`: the list's row links become child `page` doors on the same
//!   host (review M1: one mechanism for first-party lists and newsrooms).
//!
//! Cadence: a door is read at most once per [`READ_EVERY`]; a hub list page
//! therefore at most once a day.

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use reqwest::Url;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{info, warn};
use uuid::Uuid;

use super::registry::Registry;
use super::rows::{self, TierReading};
use super::{DoorFlags, Kind, HUB_TERMS_ALLOWED_SQL};
use crate::corporate_auto::bootstrap::{self, BrandScope, Scope};
use crate::corporate_auto::http::PageHttp;
use crate::corporate_auto::policy::{self, DomainSession, Fetched, LiveOptOuts, PageState};

/// A door is read at most this often.
pub const READ_EVERY: Duration = Duration::hours(24);
/// Doors read per run.
pub const MAX_DOORS_PER_RUN: i64 = 40;
/// Requests one domain may take in one run (robots included).
pub const MAX_REQUESTS_PER_DOMAIN: u32 = 12;
/// New child doors a list may add per read, and in all.
pub const MAX_NEW_CHILDREN_PER_READ: usize = 10;
pub const MAX_CHILDREN: i64 = 60;
/// A list's article read undated this many times running is retired.
pub const CHILD_UNDATED_READS: i64 = 3;
/// A list read three days running without its rows is a lost door.
pub const UNRECOGNISED_DAYS_LOST: i32 = 3;
/// A second gone read at least this long after the first marks the door
/// lost.
pub const GONE_CONFIRM_AFTER: Duration = Duration::hours(1);

#[derive(Debug, Clone, FromRow)]
pub struct DoorRow {
    pub id: Uuid,
    pub domain_id: Uuid,
    pub domain: String,
    pub entry_url: String,
    pub candidate_id: Option<Uuid>,
    pub grantor_brand_id: Uuid,
    pub grantor_name: String,
    pub kind: String,
    /// The door's lane: its own kind, or its list's for a page a list found.
    pub lane: String,
    pub parent_door_id: Option<Uuid>,
    pub max_bytes: i32,
    pub state: String,
    pub first_gone_at: Option<chrono::DateTime<Utc>>,
    pub unrecognised_days: i32,
    pub consecutive_failures: i32,
    pub last_matched_brands: Vec<Uuid>,
    pub gray_robots_missing: bool,
    pub gray_crawl_delay_min: bool,
    pub min_request_interval_ms: i32,
}

impl DoorRow {
    pub fn kind(&self) -> Option<Kind> {
        Kind::parse(&self.kind)
    }
}

/// One offer a door states, as the ledger keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerOffer {
    pub redeem_brand_id: Uuid,
    pub issuer_brand_id: Uuid,
    pub offer_key: String,
    pub value_kind: &'static str,
    pub value: String,
    pub upper_bound: bool,
    pub tiers: &'static str,
    pub quote: String,
    pub restriction: Option<String>,
    pub starts: Option<NaiveDate>,
    pub ends: Option<NaiveDate>,
}

/// What one read of a door found (pure part).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DoorRead {
    pub verdict: String,
    pub rows_seen: usize,
    pub rows_matched: usize,
    pub tiered_rows: usize,
    pub by_reason: BTreeMap<String, usize>,
    pub matched_brands: BTreeSet<Uuid>,
    pub offers: Vec<LedgerOffer>,
    /// The read states the door's whole set: an offer it does not state
    /// is gone from the door.
    pub complete: bool,
    /// Child pages a list names.
    pub links: Vec<Url>,
}

fn tiers_text(tiers: TierReading) -> &'static str {
    match tiers {
        TierReading::Untiered => "untiered",
        TierReading::EveryTierSame => "every_tier_same",
        TierReading::EveryTierMinimum => "every_tier_minimum",
    }
}

fn page_offer_key(issuer: Uuid, redeem: Uuid, facts: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("page\x1f{issuer}\x1f{redeem}"));
    for (kind, value) in facts {
        hasher.update(format!("\x1f{kind}={value}"));
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Reads one fetched door page (pure). `child` = a page found through a
/// list, which must also be dated.
pub fn interpret(
    door: &DoorRow,
    html: &str,
    page_url: &Url,
    registry: &Registry,
    today: NaiveDate,
) -> DoorRead {
    let mut read = DoorRead::default();
    if rows::nested_too_deep(html) {
        read.verdict = "page_too_deep".into();
        return read;
    }
    match door.kind() {
        Some(Kind::HubRows) => {
            let hub = rows::read_hub(html, registry, door.grantor_brand_id, today);
            read.verdict = hub.verdict().to_string();
            read.complete = hub.listing;
            read.rows_seen = hub.rows_seen;
            read.rows_matched = hub.rows_matched;
            read.tiered_rows = hub.tiered_rows;
            read.matched_brands = hub.matched_brands();
            read.by_reason = hub
                .rejected
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect();
            read.offers = hub
                .offers
                .iter()
                .map(|o| LedgerOffer {
                    redeem_brand_id: o.brand_id,
                    issuer_brand_id: door.grantor_brand_id,
                    offer_key: o.offer_key.clone(),
                    value_kind: o.kind.fact_kind(),
                    value: o.value.clone(),
                    upper_bound: o.upper_bound,
                    tiers: tiers_text(o.tiers),
                    quote: o.quote.clone(),
                    restriction: None,
                    starts: o.starts,
                    ends: o.ends,
                })
                .collect();
        }
        Some(Kind::Page) => {
            let mut brand =
                BrandScope::new(door.grantor_brand_id, &door.grantor_name, door.id, registry);
            brand.dated = door.parent_door_id.is_some();
            let scope = Scope::Brand(brand);
            read.complete = true;
            match bootstrap::extract_offer_scoped(html, today, &scope) {
                Err(reason) => read.verdict = reason.to_string(),
                Ok(offer) => {
                    let Some(first) = offer
                        .facts
                        .iter()
                        .find(|f| matches!(f.kind, "discount_percent" | "krw"))
                    else {
                        read.verdict = "no_brand_offer".into();
                        return read;
                    };
                    let issuer = offer
                        .issuer
                        .as_ref()
                        .map_or(door.grantor_brand_id, |i| i.brand_id);
                    let mut facts: Vec<(String, String)> = offer
                        .facts
                        .iter()
                        .map(|f| (f.kind.to_string(), f.value.to_string()))
                        .collect();
                    facts.sort();
                    read.verdict = "proposal".into();
                    read.matched_brands.insert(door.grantor_brand_id);
                    read.offers.push(LedgerOffer {
                        redeem_brand_id: door.grantor_brand_id,
                        issuer_brand_id: issuer,
                        offer_key: page_offer_key(issuer, door.grantor_brand_id, &facts),
                        value_kind: first.kind,
                        value: first.value.to_string(),
                        upper_bound: false,
                        tiers: "untiered",
                        quote: first.quote.chars().take(100).collect(),
                        restriction: offer.issuer.as_ref().map(|i| i.line.clone()),
                        starts: None,
                        ends: offer.end_date,
                    });
                }
            }
        }
        Some(Kind::Listing) => {
            read.links = rows::row_links(html, page_url, 40);
            read.verdict = if read.links.is_empty() {
                "listing_unrecognised".into()
            } else {
                "listing".into()
            };
            read.complete = !read.links.is_empty();
        }
        None => read.verdict = "unknown_kind".into(),
    }
    read
}

/// How a fetch ended, for health.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Html(String, Url),
    Gone,
    /// robots disallows the path now: the door is lost at once.
    Disallowed,
    /// A redirect to another host: never followed, the door is lost.
    MovedOffHost,
    OptedOut,
    /// A wall, 429, robots unreadable, budget, `Visit-time`: retried, never a
    /// health verdict on the door.
    Deferred(&'static str),
    Failed(&'static str),
}

impl Fetch {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Html(..) => "html",
            Self::Gone => "page_gone",
            Self::Disallowed => "robots_disallow",
            Self::MovedOffHost => "moved_off_host",
            Self::OptedOut => "opted_out",
            Self::Deferred(reason) | Self::Failed(reason) => reason,
        }
    }
}

pub async fn fetch_door(
    session: &mut DomainSession<'_>,
    door: &DoorRow,
    opted_out: Option<&dyn policy::OptOutCheck>,
) -> Fetch {
    let Ok(url) = Url::parse(&door.entry_url) else {
        return Fetch::Failed("invalid_url");
    };
    if let Some(check) = opted_out {
        if check.covers(&url).await {
            return Fetch::OptedOut;
        }
    }
    let resolved = policy::fetch_resolving_same_host_limited(
        session,
        &url,
        door.gray_robots_missing,
        opted_out,
        door.max_bytes.max(65_536) as usize,
    )
    .await;
    match (&resolved.state, &resolved.fetched) {
        (PageState::Html { text_chars }, Fetched::Reply(reply)) => {
            if *text_chars < policy::JS_SHELL_CHARS {
                return Fetch::Failed("js_shell");
            }
            match crate::sources::benefit_pages::decode_page(&reply.body, &reply.content_type) {
                Ok(html) => Fetch::Html(html, resolved.url.clone()),
                Err(_) => Fetch::Failed("unsupported_content"),
            }
        }
        (PageState::Gone, _) => Fetch::Gone,
        (PageState::Disallowed, _) => Fetch::Disallowed,
        (
            PageState::Redirect {
                same_host: false, ..
            },
            _,
        ) => Fetch::MovedOffHost,
        (PageState::Redirect { .. }, _) => Fetch::Failed("redirect_loop"),
        (PageState::BotWall, _) => Fetch::Deferred("bot_wall"),
        (PageState::RateLimited(_), _) => Fetch::Deferred("rate_limited"),
        (PageState::Unsupported, _) => Fetch::Failed("unsupported_content"),
        (PageState::Transient, _) => Fetch::Failed("page_unreachable"),
        (PageState::NotProbed, Fetched::Stopped("opt_out")) => Fetch::OptedOut,
        (PageState::NotProbed, Fetched::Stopped(reason)) => Fetch::Deferred(reason),
        (PageState::NotProbed, Fetched::BudgetExhausted) => Fetch::Deferred("request_budget"),
        (PageState::NotProbed, _) => Fetch::Failed("page_unreachable"),
        (PageState::Html { .. }, _) => Fetch::Failed("unexpected_reply"),
    }
}

/// The doors due now: not retired, the domain policy allows reading
/// (`policy::DOMAIN_POLICY_SQL`), a hub only with its terms recorded as
/// allowing it, a flagged door only with its flag on. `$1` flags on, `$2`
/// limit.
const DUE_DOORS_SQL: &str = "SELECT dr.id
   FROM corporate_doors dr
   JOIN benefit_source_domains d ON d.id = dr.domain_id
   JOIN brands b ON b.id = dr.grantor_brand_id AND b.is_active
  WHERE dr.state <> 'retired' AND dr.next_read_at <= now()
    AND {policy}
    AND (dr.kind <> 'hub_rows' OR {hub_terms})
    AND (dr.flag IS NULL OR dr.flag = ANY($1))
    AND ($3::uuid[] IS NULL OR dr.id = ANY($3))
  ORDER BY dr.next_read_at, dr.id
  LIMIT $2";

pub async fn due_doors(
    pool: &PgPool,
    flags: &DoorFlags,
    limit: i64,
    only: Option<&[Uuid]>,
) -> Result<Vec<DoorRow>> {
    // Claim before the fetch (security review M1, code review M-8): the
    // due doors move a day ahead in one statement, locked rows skipped, so
    // an overlapping run, a crash after the fetch or a failed record never
    // reads a door twice in a day. `update_health` shortens it afterwards
    // only for a first gone read and a closed visit window.
    let claimed: Vec<Uuid> = sqlx::query_scalar(&format!(
        "WITH due AS ({select} FOR UPDATE OF dr SKIP LOCKED)
         UPDATE corporate_doors c SET next_read_at = now() + interval '24 hours', updated_at = now()
         FROM due WHERE c.id = due.id
         RETURNING c.id",
        select = DUE_DOORS_SQL
            .replace("{policy}", policy::DOMAIN_POLICY_SQL)
            .replace("{hub_terms}", HUB_TERMS_ALLOWED_SQL)
    ))
    .bind(&flags.flags_on)
    .bind(limit)
    .bind(only)
    .fetch_all(pool)
    .await
    .context("failed to claim due doors")?;
    sqlx::query_as::<_, DoorRow>(
        "SELECT dr.id, dr.domain_id, d.domain, dr.entry_url, dr.candidate_id,
                dr.grantor_brand_id, b.name AS grantor_name, dr.kind,
                COALESCE((SELECT p.kind FROM corporate_doors p WHERE p.id = dr.parent_door_id), dr.kind) AS lane,
                dr.parent_door_id, dr.max_bytes, dr.state, dr.first_gone_at, dr.unrecognised_days,
                dr.consecutive_failures, dr.last_matched_brands, d.gray_robots_missing, d.gray_crawl_delay_min,
                d.min_request_interval_ms
         FROM corporate_doors dr
         JOIN benefit_source_domains d ON d.id = dr.domain_id
         JOIN brands b ON b.id = dr.grantor_brand_id
         WHERE dr.id = ANY($1)
         ORDER BY dr.id",
    )
    .bind(&claimed)
    .fetch_all(pool)
    .await
    .context("failed to load claimed doors")
}

/// Summary of one run.
#[derive(Debug, Default, Clone)]
pub struct RunSummary {
    pub doors: usize,
    pub verdicts: BTreeMap<String, usize>,
    pub offers_seen: usize,
    pub published: usize,
    pub hidden: usize,
    pub requests: u32,
}

/// One pass over the due doors (`only`: these doors alone, for tests).
pub async fn run_with(
    pool: &PgPool,
    flags: &DoorFlags,
    http: &dyn PageHttp,
    test_spacing: Option<std::time::Duration>,
    only: Option<&[Uuid]>,
) -> Result<RunSummary> {
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "the door job only sends the MazimiBenefitBot user agent"
    );
    let mut summary = RunSummary::default();
    if !flags.reads() {
        return Ok(summary);
    }
    let registry = std::sync::Arc::new(Registry::load(pool).await?);
    let today = crate::corporate_auto::health::kst_today();
    let mut doors = due_doors(pool, flags, MAX_DOORS_PER_RUN, only).await?;
    doors.sort_by_key(|d| d.domain_id);
    let optouts = LiveOptOuts(pool);
    let mut publish_budget = crate::corporate_auto::intake::MAX_PUBLISH_PER_RUN;
    let mut index = 0;
    while index < doors.len() {
        let domain_id = doors[index].domain_id;
        let end = doors[index..]
            .iter()
            .position(|d| d.domain_id != domain_id)
            .map_or(doors.len(), |o| index + o);
        let group = doors[index..end].to_vec();
        index = end;
        let shared = test_spacing.is_none();
        let first = &group[0];
        let mut session = DomainSession::new(
            http,
            shared.then_some(pool),
            shared.then_some(domain_id),
            first.gray_crawl_delay_min,
            first.min_request_interval_ms as i64,
            MAX_REQUESTS_PER_DOMAIN,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        for door in &group {
            let before = session.requests();
            let mut fetch = fetch_door(&mut session, door, Some(&optouts)).await;
            let requests = session.requests() - before;
            // Reading a page is CPU work on untrusted input: off the async
            // threads (security review M2). A reader that panics is a
            // failed read, never a verdict on the door.
            let read = match &fetch {
                Fetch::Html(html, url) => {
                    let (door, html, url, registry) =
                        (door.clone(), html.clone(), url.clone(), registry.clone());
                    match tokio::task::spawn_blocking(move || {
                        interpret(&door, &html, &url, &registry, today)
                    })
                    .await
                    {
                        Ok(read) => Some(read),
                        Err(_) => {
                            fetch = Fetch::Failed("read_failed");
                            None
                        }
                    }
                }
                _ => None,
            };
            let verdict = read
                .as_ref()
                .map_or_else(|| fetch.label().to_string(), |r| r.verdict.clone());
            *summary.verdicts.entry(verdict.clone()).or_default() += 1;
            summary.doors += 1;
            if let Err(error) =
                record(pool, flags, door, &fetch, read.as_ref(), requests, today).await
            {
                warn!(event = "corporate_doors.record_failed", door_id = %door.id,
                      error = %crate::error_safety::persisted_error(&error), "door read could not be recorded");
                continue;
            }
            if let Some(read) = &read {
                summary.offers_seen += read.offers.len();
            }
            if door.kind() == Some(Kind::HubRows) && flags.publishes(Kind::HubRows) {
                match super::lifecycle::publish_hub(pool, door, &fetch, today, &mut publish_budget)
                    .await
                {
                    Ok((published, hidden)) => {
                        summary.published += published;
                        summary.hidden += hidden;
                    }
                    Err(error) => {
                        warn!(event = "corporate_doors.publish_failed", door_id = %door.id,
                                        error = %crate::error_safety::persisted_error(&error), "hub publish failed")
                    }
                }
            }
        }
        summary.requests += session.requests();
    }
    super::lifecycle::retire_unreplaced(pool).await?;
    Ok(summary)
}

/// Writes the read, the ledger and the door's health in one transaction.
async fn record(
    pool: &PgPool,
    flags: &DoorFlags,
    door: &DoorRow,
    fetch: &Fetch,
    read: Option<&DoorRead>,
    requests: u32,
    today: NaiveDate,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let mode = if Kind::parse(&door.lane).is_some_and(|lane| flags.publishes(lane)) {
        "live"
    } else {
        "shadow"
    };
    let verdict = read.map_or_else(|| fetch.label().to_string(), |r| r.verdict.clone());
    // Identical to the previous read: the same offers with the same quotes.
    let previous: Vec<(String, String)> = sqlx::query_as(
        "SELECT offer_key, quote FROM corporate_door_offers
         WHERE door_id = $1 AND missing_since IS NULL AND status <> 'gone' ORDER BY offer_key",
    )
    .bind(door.id)
    .fetch_all(&mut *tx)
    .await?;
    let identical = read.filter(|r| r.complete).map(|r| {
        let mut now: Vec<(String, String)> = r
            .offers
            .iter()
            .map(|o| (o.offer_key.clone(), o.quote.clone()))
            .collect();
        now.sort();
        now == previous
    });
    let by_reason = read.map_or_else(|| json!({}), |r| json!(r.by_reason));
    let matched: Vec<Uuid> = read
        .map(|r| r.matched_brands.iter().copied().take(400).collect())
        .unwrap_or_default();
    sqlx::query(
        "INSERT INTO corporate_door_reads (door_id, mode, verdict, rows_seen, rows_matched, rows_accepted,
                                           tiered_rows, by_reason, matched_brands, identical_to_previous, requests)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(door.id)
    .bind(mode)
    .bind(verdict.chars().take(60).collect::<String>())
    .bind(read.map_or(0, |r| r.rows_seen as i32))
    .bind(read.map_or(0, |r| r.rows_matched as i32))
    .bind(read.map_or(0, |r| r.offers.len() as i32))
    .bind(read.map_or(0, |r| r.tiered_rows as i32))
    .bind(by_reason)
    .bind(&matched)
    .bind(identical)
    .bind(requests as i32)
    .execute(&mut *tx)
    .await?;
    if let Some(read) = read {
        for offer in &read.offers {
            sqlx::query(
                "INSERT INTO corporate_door_offers (door_id, redeem_brand_id, offer_key, issuer_brand_id, value_kind,
                                                    value, upper_bound, tiers, quote, restriction, starts_on, ends_on)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                 ON CONFLICT (door_id, redeem_brand_id, offer_key) DO UPDATE SET
                     sightings = corporate_door_offers.sightings + 1,
                     rereads = corporate_door_offers.rereads + 1,
                     rereads_identical = corporate_door_offers.rereads_identical
                         + (corporate_door_offers.quote = EXCLUDED.quote)::int,
                     quote = EXCLUDED.quote, restriction = EXCLUDED.restriction,
                     tiers = EXCLUDED.tiers, starts_on = EXCLUDED.starts_on, ends_on = EXCLUDED.ends_on,
                     last_seen_at = now(), missing_since = NULL,
                     -- A returning offer starts its stability again.
                     first_seen_at = CASE WHEN corporate_door_offers.status = 'gone' THEN now()
                                          ELSE corporate_door_offers.first_seen_at END,
                     status = CASE WHEN corporate_door_offers.status = 'gone' THEN 'observed'
                                   ELSE corporate_door_offers.status END",
            )
            .bind(door.id)
            .bind(offer.redeem_brand_id)
            .bind(&offer.offer_key)
            .bind(offer.issuer_brand_id)
            .bind(offer.value_kind)
            .bind(&offer.value)
            .bind(offer.upper_bound)
            .bind(offer.tiers)
            .bind(offer.quote.chars().take(100).collect::<String>())
            .bind(offer.restriction.as_ref().map(|r| r.chars().take(100).collect::<String>()))
            .bind(offer.starts)
            .bind(offer.ends)
            .execute(&mut *tx)
            .await?;
        }
        if read.complete {
            let keys: Vec<String> = read.offers.iter().map(|o| o.offer_key.clone()).collect();
            sqlx::query(
                "UPDATE corporate_door_offers SET missing_since = COALESCE(missing_since, now())
                 WHERE door_id = $1 AND NOT (offer_key = ANY($2))",
            )
            .bind(door.id)
            .bind(&keys)
            .execute(&mut *tx)
            .await?;
        }
        if door.kind() == Some(Kind::Listing) {
            add_children(&mut tx, door, &read.links).await?;
        }
        // A page whose lane is live gets its candidate: intake then reads
        // and publishes it under the door's scope through its whole chain.
        if door.kind() == Some(Kind::Page)
            && Kind::parse(&door.lane).is_some_and(|lane| flags.publishes(lane))
            && read.verdict == "proposal"
        {
            super::lifecycle::door_candidate(&mut tx, door).await?;
        }
    }
    update_health(&mut tx, door, fetch, read, today).await?;
    tx.commit().await?;
    Ok(())
}

async fn add_children(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    door: &DoorRow,
    links: &[Url],
) -> Result<()> {
    // The cap counts active children only (review H-4): a retired article
    // leaves room for the next one; its URL is never added again.
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_doors WHERE parent_door_id = $1 AND state <> 'retired'",
    )
    .bind(door.id)
    .fetch_one(&mut **tx)
    .await?;
    let room = (MAX_CHILDREN - existing).clamp(0, MAX_NEW_CHILDREN_PER_READ as i64) as usize;
    let mut added = 0;
    for link in links {
        if added >= room {
            break;
        }
        // Same host as the door, never another (review M4): row_links only
        // returns such links; checked again here. A link with a port or
        // userinfo is never stored (security review L3).
        if link.port().is_some() || !link.username().is_empty() || link.password().is_some() {
            continue;
        }
        if link.host_str()
            != Url::parse(&door.entry_url)
                .ok()
                .as_ref()
                .and_then(|u| u.host_str())
        {
            continue;
        }
        let inserted = sqlx::query(
            "INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind, parent_door_id)
             VALUES ($1, $2, $3, 'page', $4) ON CONFLICT (entry_url) DO NOTHING",
        )
        .bind(door.domain_id)
        .bind(link.as_str())
        .bind(door.grantor_brand_id)
        .bind(door.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        added += inserted as usize;
    }
    Ok(())
}

/// The door's health after a read (review M4, M6): a good read clears it;
/// robots disallow or a move off the host loses it at once; two gone reads
/// at least [`GONE_CONFIRM_AFTER`] apart, or a list unread
/// [`UNRECOGNISED_DAYS_LOST`] reads running, lose it. A deferred read
/// changes nothing but the schedule.
async fn update_health(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    door: &DoorRow,
    fetch: &Fetch,
    read: Option<&DoorRead>,
    _today: NaiveDate,
) -> Result<()> {
    let now = Utc::now();
    let unrecognised = read.is_some_and(|r| r.verdict == "listing_unrecognised");
    let (lost, outcome): (bool, String) = match fetch {
        Fetch::Html(..) if unrecognised => (
            door.unrecognised_days + 1 >= UNRECOGNISED_DAYS_LOST,
            "listing_unrecognised".into(),
        ),
        Fetch::Html(..) => (false, read.map_or("html".into(), |r| r.verdict.clone())),
        Fetch::Disallowed | Fetch::MovedOffHost => (true, fetch.label().into()),
        Fetch::Gone => (
            door.first_gone_at
                .is_some_and(|first| now - first >= GONE_CONFIRM_AFTER),
            "page_gone".into(),
        ),
        other => (false, other.label().into()),
    };
    let next = match fetch {
        Fetch::Gone if door.first_gone_at.is_none() => now + GONE_CONFIRM_AFTER,
        Fetch::Deferred("visit_window") => now + Duration::hours(1),
        _ => now + READ_EVERY,
    };
    let success = matches!(fetch, Fetch::Html(..)) && !unrecognised;
    let became_lost = lost && door.state == "active";
    // A list's article whose verdict is final is retired with it (review
    // H-4): too old, stale or ended at once (an article never gets
    // younger), undated after three reads running.
    if door.parent_door_id.is_some() {
        let verdict = read.map(|r| r.verdict.as_str()).unwrap_or("");
        let terminal = match verdict {
            "article_too_old" | "stale_page" | "offer_ended" | "article_end_implausible" => true,
            "article_undated" => {
                let undated: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM (SELECT verdict FROM corporate_door_reads
                      WHERE door_id = $1 ORDER BY read_at DESC LIMIT $2) r
                     WHERE r.verdict = 'article_undated'",
                )
                .bind(door.id)
                .bind(CHILD_UNDATED_READS)
                .fetch_one(&mut **tx)
                .await?;
                undated >= CHILD_UNDATED_READS
            }
            _ => false,
        };
        if terminal {
            sqlx::query(
                "UPDATE corporate_doors SET state = 'retired', last_attempt_at = now(), last_outcome = $2,
                                            updated_at = now()
                 WHERE id = $1",
            )
            .bind(door.id)
            .bind(verdict)
            .execute(&mut **tx)
            .await?;
            return Ok(());
        }
    }
    sqlx::query(
        "UPDATE corporate_doors SET
             last_attempt_at = now(),
             last_success_at = CASE WHEN $2 THEN now() ELSE last_success_at END,
             consecutive_failures = CASE WHEN $2 THEN 0 ELSE LEAST(consecutive_failures + 1, 1000) END,
             first_gone_at = CASE WHEN $3 THEN COALESCE(first_gone_at, now()) ELSE NULL END,
             unrecognised_days = CASE WHEN $4 THEN LEAST(unrecognised_days + 1, 1000) ELSE 0 END,
             state = CASE WHEN $5 THEN 'lost' WHEN $2 THEN 'active' ELSE state END,
             lost_since = CASE WHEN $5 THEN COALESCE(lost_since, now()) WHEN $2 THEN NULL ELSE lost_since END,
             last_outcome = $6,
             last_matched_brands = CASE WHEN $2 AND cardinality($7::uuid[]) > 0 THEN $7 ELSE last_matched_brands END,
             next_read_at = $8,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(door.id)
    .bind(success)
    .bind(matches!(fetch, Fetch::Gone))
    .bind(unrecognised)
    .bind(lost)
    .bind(outcome.chars().take(60).collect::<String>())
    .bind(
        read.map(|r| r.matched_brands.iter().copied().take(400).collect::<Vec<_>>())
            .unwrap_or_default(),
    )
    .bind(next)
    .execute(&mut **tx)
    .await?;
    if became_lost {
        crate::operations::record_event_tx(
            tx,
            &crate::operations::EventCommand {
                fingerprint: &format!("corporate_door_lost:{}", door.id),
                severity: crate::operations::Severity::Warning,
                component: "corporate_doors",
                event_type: "corporate_door_lost",
                subject_type: Some("corporate_door"),
                subject_id: Some(&door.id.to_string()),
                message: "A corporate door stopped reading; the server looks for its replacement on the same host.",
                details: json!({"kind": door.kind, "domain": door.domain, "outcome": outcome}),
            },
        )
        .await?;
    }
    Ok(())
}

/// Worker entry point; logs instead of failing the scheduler. Returns at
/// once, with no query, while `CORPORATE_DOORS_MODE` is off.
pub async fn run_scheduled(pool: &PgPool) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _release = Release;
    // The hide sweep runs whatever the switches say (database only, no
    // request): turning doors off never leaves a hub offer shown.
    match super::lifecycle::sweep(pool, crate::corporate_auto::health::kst_today()).await {
        Ok(0) => {}
        Ok(hidden) => info!(
            event = "corporate_doors.sweep",
            hidden, "door offers hidden or held by the sweep"
        ),
        Err(error) => warn!(event = "corporate_doors.sweep_failed",
                            error = %crate::error_safety::persisted_error(&error), "door hide sweep failed"),
    }
    let flags = match DoorFlags::from_env() {
        Ok(flags) if flags.reads() => flags,
        Ok(_) => return,
        Err(error) => {
            warn!(event = "corporate_doors.config_invalid", error = %error, "corporate doors misconfigured");
            return;
        }
    };
    let Ok(Some(user_agent)) = crate::corporate_auto::Flags::from_env().map(|f| f.user_agent())
    else {
        warn!(
            event = "corporate_doors.config_invalid",
            "CORPORATE_BOT_CONTACT_URL is required for the door job"
        );
        return;
    };
    let http = match crate::corporate_auto::http::PinnedHttp::new(
        user_agent,
        std::time::Duration::from_secs(20),
    ) {
        Ok(http) => http,
        Err(error) => {
            warn!(event = "corporate_doors.config_invalid", error = %error, "door transport rejected");
            return;
        }
    };
    match run_with(pool, &flags, &http, None, None).await {
        Ok(summary) => {
            info!(event = "corporate_doors.run", mode = flags.mode.as_str(), doors = summary.doors,
                             offers = summary.offers_seen, published = summary.published, hidden = summary.hidden,
                             requests = summary.requests, verdicts = ?summary.verdicts, "door pass complete")
        }
        Err(error) => warn!(event = "corporate_doors.run_failed",
                            error = %crate::error_safety::persisted_error(&error), "door pass failed"),
    }
    match super::lifecycle::rediscover(pool, &flags, &http, None, None).await {
        Ok(found) => info!(
            event = "corporate_doors.rediscovery",
            replaced = found,
            "door rediscovery pass complete"
        ),
        Err(error) => warn!(event = "corporate_doors.rediscovery_failed",
                            error = %crate::error_safety::persisted_error(&error), "door rediscovery failed"),
    }
}

/// The read scope of each candidate a door owns (review H5): one query and
/// one registry load for a batch. A candidate no door owns reads under
/// [`Scope::Audience`]. Every reader of a candidate (intake, fresh
/// extraction, the weekly audit) takes its scope from here, so an offer
/// read under brand scope is audited and re-adopted under the same scope.
pub async fn scopes_for(
    pool: &PgPool,
    candidates: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Scope>> {
    let rows: Vec<(Uuid, Uuid, Uuid, String, bool)> = sqlx::query_as(
        "SELECT dr.candidate_id, dr.id, dr.grantor_brand_id, b.name, dr.parent_door_id IS NOT NULL
         FROM corporate_doors dr JOIN brands b ON b.id = dr.grantor_brand_id
         WHERE dr.candidate_id = ANY($1) AND dr.kind <> 'hub_rows'",
    )
    .bind(candidates)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(Default::default());
    }
    let registry = Registry::load(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(candidate, door, brand, name, dated)| {
            let mut scope = BrandScope::new(brand, &name, door, &registry);
            scope.dated = dated;
            (candidate, Scope::Brand(scope))
        })
        .collect())
}
