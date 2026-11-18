//! What happens to doors and their offers over time.
//!
//! **Hub offers (live)** — [`publish_hub`], only while `hub_rows` is a live
//! kind and intake is live, after a read of the hub:
//! - an offer seen on two reads at least [`STABLE_AFTER`] apart becomes a
//!   programme of its grantor (review H3: provider, company and sponsor
//!   are the grantor; the redeem brand is `redeemable_at`; no logo), titled
//!   by the template, with the condition from the grantor's holder key plus
//!   the row's verbatim restriction (C2), ending at its stated end (KST);
//! - the same (issuer, redeem brand, value kind) already published by
//!   another door is not published twice (H6);
//! - the same row with a new period moves the end date (H8);
//! - a row a complete read no longer states, an opt-out on the hub or on
//!   the redeem brand's domain, hides the programme at once;
//! - an offer no live read has confirmed for 3 days is hidden by
//!   [`sweep`], which runs on every worker tick whatever the switches say;
//!   the existing fail-closed overdue sweep holds it too (its candidate is
//!   `approved`).
//!
//! The door job is a hub programme's evidence: its monitor is
//! `verification_mode = 'door'`, which no other reader verifies, and the
//! hub's domain is outside `ELIGIBLE_DOMAIN_SQL`.
//!
//! **Lost doors** (review M6) — [`rediscover`] looks for a replacement on
//! the same host only (no host is ever registered, M4): the robots
//! `Sitemap:` lines and `/sitemap.xml`, the URLs closest to the lost one by
//! path, at most [`REDISCOVERY_FETCHES`] a day, accepted only when the new
//! page reads as the same door (a hub: at least half of its last brands; a
//! page: one of its offers; a list: a list of links). A door lost
//! [`RETIRE_AFTER_DAYS`] days with no replacement is retired as
//! `lost_no_replacement_30d`, a counted outcome; its brands stay covered by
//! any other door.

use anyhow::Result;
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use reqwest::Url;
use serde_json::json;
use sqlx::{FromRow, PgPool};
use std::collections::BTreeSet;
use uuid::Uuid;

use super::job::{self, DoorRow, Fetch};
use super::registry::Registry;
use super::{DoorFlags, Kind};
use crate::corporate_auto::http::PageHttp;
use crate::corporate_auto::policy::{DomainSession, LiveOptOuts};

/// Two reads at least this far apart before a hub offer is shown.
pub const STABLE_AFTER: Duration = Duration::hours(24);
pub const RETIRE_AFTER_DAYS: i64 = 30;
pub const REDISCOVERY_FETCHES: usize = 3;
/// Lost doors examined per run.
const REDISCOVERY_DOORS_PER_RUN: i64 = 5;

fn end_of_kst_day(day: NaiveDate) -> Option<chrono::DateTime<Utc>> {
    let kst = chrono::FixedOffset::east_opt(9 * 3600)?;
    kst.from_local_datetime(&day.and_hms_opt(23, 59, 59)?)
        .single()
        .map(|at| at.with_timezone(&Utc))
}

#[derive(Debug, FromRow)]
struct LedgerRow {
    id: Uuid,
    redeem_brand_id: Uuid,
    offer_key: String,
    value_kind: String,
    value: String,
    upper_bound: bool,
    tiers: String,
    quote: String,
    restriction: Option<String>,
    ends_on: Option<NaiveDate>,
    program_id: Option<Uuid>,
    status: String,
    missing_since: Option<chrono::DateTime<Utc>>,
    first_seen_at: chrono::DateTime<Utc>,
    sightings: i32,
    starts_on: Option<NaiveDate>,
}

/// The display condition (review C2): the grantor's holder template, then
/// the row's restriction verbatim.
pub fn condition(
    holder: &mazimi_core::holder::Holder,
    grantor: &str,
    restriction: Option<&str>,
) -> String {
    let template = holder.field.condition_template(grantor);
    match restriction.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("{template} · {r}"),
        None => template,
    }
}

/// Display-scope opt-outs that cover a door offer (security review H5,
/// code review M-10): one on the hub's domain, on any source domain of the
/// grantor or the redeem brand, or proved on a host that is the grantor's
/// or the redeem brand's own registry domain (a brand with no source domain
/// of ours can still opt out). `$1` hub domain id, `$2` redeem brand id,
/// `$3` grantor brand id.
pub(crate) const OPTED_OUT_SQL: &str = "SELECT EXISTS (
    SELECT 1 FROM benefit_source_optouts o
    WHERE o.lifted_at IS NULL AND o.effective_at IS NOT NULL AND o.scope IN ('display', 'both')
      AND ($1 = ANY(o.domain_ids)
           OR EXISTS (SELECT 1 FROM benefit_source_domain_brands db
                      WHERE db.brand_id IN ($2, $3) AND db.domain_id = ANY(o.domain_ids))
           OR EXISTS (SELECT 1 FROM brand_discovery_targets t
                      WHERE t.brand_id IN ($2, $3) AND t.official_domain IS NOT NULL AND o.domain IS NOT NULL
                        AND regexp_replace(lower(t.official_domain), '^www\\.', '')
                            = regexp_replace(lower(o.domain), '^www\\.', ''))))";

async fn hide(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ledger: Uuid,
    program: Option<Uuid>,
) -> Result<()> {
    sqlx::query("UPDATE corporate_door_offers SET status = 'gone' WHERE id = $1")
        .bind(ledger)
        .execute(&mut **tx)
        .await?;
    if let Some(program) = program {
        // The identity is free again: another door, or intake, may show it.
        sqlx::query("DELETE FROM corporate_benefit_identities WHERE program_id = $1")
            .bind(program)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "UPDATE programs SET is_active = false, program_status = 'closed', updated_at = now()
             WHERE id = $1 AND is_active",
        )
        .bind(program)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE program_source_monitors SET verification_state = 'retired', updated_at = now()
             WHERE program_id = $1 AND verification_mode = 'door'",
        )
        .bind(program)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// The candidate a door's programmes are monitored through (created once).
/// Always `approved` and an index, so the existing fail-closed sweep
/// (`source_monitoring::hold_overdue_sources`) holds its programmes once
/// their `door` monitor is overdue, whatever the door switches say (code
/// review C-1). A candidate another lane found first at the same URL (the
/// KB hub's `search_api` row in `review`, deploy report §9) is adopted into
/// that state, keeping its provenance; a linked candidate another lane
/// moved out of it is moved back. No other reader fetches it
/// (`policy::hub_owned`).
pub(crate) async fn door_candidate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    door: &DoorRow,
) -> Result<Uuid> {
    if let Some(id) = door.candidate_id {
        sqlx::query(DOOR_CANDIDATE_STATE_SQL)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        return Ok(id);
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, status,
                                                candidate_kind, next_check_at, recheck_interval_seconds)
         VALUES ($1, $2, $2, 'official_page', 'approved', 'index', NULL, 86400)
         ON CONFLICT (canonical_url) DO UPDATE SET last_discovered_at = now()
         RETURNING id",
    )
    .bind(door.domain_id)
    .bind(&door.entry_url)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(DOOR_CANDIDATE_STATE_SQL)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE corporate_doors SET candidate_id = $2 WHERE id = $1 AND candidate_id IS NULL",
    )
    .bind(door.id)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// The state a door-owned candidate is kept in ([`door_candidate`]): an
/// approved index. A candidate entering it has no schedule of its own yet
/// (the monitors' reconcile sets one); one already in it keeps its schedule.
const DOOR_CANDIDATE_STATE_SQL: &str = "UPDATE benefit_source_candidates
     SET status = 'approved', candidate_kind = 'index', next_check_at = NULL, updated_at = now()
     WHERE id = $1 AND (status <> 'approved' OR candidate_kind <> 'index')";

/// Publishes, refreshes and hides one hub door's offers after a read.
/// Returns (published, hidden). Does nothing for a read that did not
/// return the page: [`sweep`] handles those.
pub async fn publish_hub(
    pool: &PgPool,
    door: &DoorRow,
    fetch: &Fetch,
    today: NaiveDate,
    budget: &mut usize,
) -> Result<(usize, usize)> {
    let (mut published, mut hidden) = (0, 0);
    if !matches!(fetch, Fetch::Html(..)) {
        if matches!(fetch, Fetch::OptedOut) {
            let mut tx = pool.begin().await?;
            let rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
                "SELECT id, program_id FROM corporate_door_offers WHERE door_id = $1 AND status = 'published'",
            )
            .bind(door.id)
            .fetch_all(&mut *tx)
            .await?;
            for (id, program) in rows {
                hide(&mut tx, id, program).await?;
                hidden += 1;
            }
            tx.commit().await?;
        }
        return Ok((published, hidden));
    }
    let (grantor_holder,): (Option<String>,) = sqlx::query_as(
        "SELECT CASE WHEN holder_field IS NOT NULL THEN holder_field || ':' || holder_value END
         FROM brands WHERE id = $1",
    )
    .bind(door.grantor_brand_id)
    .fetch_one(pool)
    .await?;
    let Some(holder) = grantor_holder
        .as_deref()
        .and_then(mazimi_core::holder::Holder::parse)
    else {
        // A hub's grantor must be an issuer: without a holder key nothing
        // can say who the offer is for.
        return Ok((0, 0));
    };
    let rows: Vec<LedgerRow> = sqlx::query_as(
        "SELECT o.id, o.redeem_brand_id, o.offer_key, o.value_kind, o.value,
                o.upper_bound, o.tiers, o.quote, o.restriction, o.ends_on, o.program_id, o.status,
                o.missing_since, o.first_seen_at, o.sightings, o.starts_on
         FROM corporate_door_offers o JOIN brands b ON b.id = o.redeem_brand_id AND b.is_active
         WHERE o.door_id = $1 AND o.status <> 'gone'
         ORDER BY o.first_seen_at, o.id",
    )
    .bind(door.id)
    .fetch_all(pool)
    .await?;
    for row in rows {
        let (status, missing_since, first_seen_at, sightings, starts_on) = (
            row.status.clone(),
            row.missing_since,
            row.first_seen_at,
            row.sightings,
            row.starts_on,
        );
        let mut tx = pool.begin().await?;
        let opted_out: bool = sqlx::query_scalar(OPTED_OUT_SQL)
            .bind(door.domain_id)
            .bind(row.redeem_brand_id)
            .bind(door.grantor_brand_id)
            .fetch_one(&mut *tx)
            .await?;
        let ended = row.ends_on.is_some_and(|end| end < today);
        if status == "published" {
            if missing_since.is_some() || opted_out || ended {
                hide(&mut tx, row.id, row.program_id).await?;
                hidden += 1;
            } else if let Some(program) = row.program_id {
                // Confirmed by this read: the end date moves with the row (H8).
                sqlx::query(
                    "UPDATE programs SET benefit_valid_until = $2, source_verified_at = now(), updated_at = now()
                     WHERE id = $1",
                )
                .bind(program)
                .bind(row.ends_on.and_then(end_of_kst_day))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE program_source_monitors SET verified_at = now(), review_due_at = now() + interval '3 days',
                                                        updated_at = now()
                     WHERE program_id = $1 AND verification_mode = 'door' AND verification_state = 'current'",
                )
                .bind(program)
                .execute(&mut *tx)
                .await?;
                // A hold the overdue sweep placed while no live read came is
                // lifted by this confirming read.
                let released = sqlx::query(
                    "UPDATE program_source_holds SET resolved_at = now(), resolution = 'confirmed_unchanged'
                     WHERE program_id = $1 AND resolved_at IS NULL AND hold_reason = 'verification_overdue'",
                )
                .bind(program)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if released > 0 {
                    sqlx::query(
                        "UPDATE programs SET is_active = true, program_status = 'always_open', source_health = 'current',
                                             retired_at = NULL, retirement_reason = NULL, updated_at = now()
                         WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM program_source_holds h
                                                       WHERE h.program_id = $1 AND h.resolved_at IS NULL)",
                    )
                    .bind(program)
                    .execute(&mut *tx)
                    .await?;
                    if let Some(candidate) = door.candidate_id {
                        sqlx::query("UPDATE benefit_source_candidates SET status = 'approved', updated_at = now() WHERE id = $1")
                            .bind(candidate)
                            .execute(&mut *tx)
                            .await?;
                    }
                }
            }
            tx.commit().await?;
            continue;
        }
        let stable = sightings >= 2 && Utc::now() - first_seen_at >= STABLE_AFTER;
        let started = starts_on.is_none_or(|s| s <= today);
        if *budget == 0 || !stable || !started || ended || missing_since.is_some() || opted_out {
            tx.commit().await?;
            continue;
        }
        let candidate = door_candidate(&mut tx, door).await?;
        let program = write_programme(&mut tx, door, &row, &holder, candidate).await?;
        // H6 / code review H-3: one shown programme per (issuer, redeem
        // brand, value kind), whichever path publishes it first.
        if !claim_identity(
            &mut tx,
            door.grantor_brand_id,
            row.redeem_brand_id,
            &row.value_kind,
            program,
        )
        .await?
        {
            tx.rollback().await?;
            continue;
        }
        sqlx::query(
            "UPDATE corporate_door_offers SET status = 'published', program_id = $2 WHERE id = $1",
        )
        .bind(row.id)
        .bind(program)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        *budget -= 1;
        published += 1;
    }
    Ok((published, hidden))
}

/// One hub offer as a programme (review H3, C2), shown at once: two reads
/// a day apart stated it, and the door job re-reads it daily.
async fn write_programme(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    door: &DoorRow,
    row: &LedgerRow,
    holder: &mazimi_core::holder::Holder,
    candidate: Uuid,
) -> Result<Uuid> {
    let tiers = match row.tiers.as_str() {
        "every_tier_minimum" => super::rows::TierReading::EveryTierMinimum,
        "every_tier_same" => super::rows::TierReading::EveryTierSame,
        _ => super::rows::TierReading::Untiered,
    };
    let title = super::rows::title_for(&door.grantor_name, &row.quote, tiers);
    let condition = condition(holder, &door.grantor_name, row.restriction.as_deref());
    let summary = match row.restriction.as_deref() {
        Some(r) => format!("{} ({r})", row.quote),
        None => row.quote.clone(),
    };
    let source_id = format!("door:{}:{}", door.id, &row.offer_key[..24]);
    let program: Uuid = match row.program_id {
        Some(existing) => {
            sqlx::query(
                "UPDATE programs SET is_active = true, program_status = 'always_open', source_health = 'current',
                                     retired_at = NULL, retirement_reason = NULL,
                                     title = $2, summary = $3, eligibility_condition = $4,
                                     benefit_valid_until = $5, source_verified_at = now(), updated_at = now()
                 WHERE id = $1",
            )
            .bind(existing)
            .bind(&title)
            .bind(&summary)
            .bind(&condition)
            .bind(row.ends_on.and_then(end_of_kst_day))
            .execute(&mut **tx)
            .await?;
            existing
        }
        None => sqlx::query_scalar(
            "INSERT INTO programs (program_type, source_type, source_id, title, summary, provider_name, company_name,
                                   official_url, program_status, provider_kind, eligibility_condition,
                                   regions, is_active, source_health, source_verified_at, benefit_valid_until,
                                   last_synced_at, normalized_payload, raw_payload)
             VALUES ('corporate_benefit', 'corporate_official', $1, $2, $3, $4, $4, $5, 'always_open', 'corporate',
                     $6, ARRAY[]::text[], true, 'current', now(), $7, now(), $8, $9)
             ON CONFLICT (source_type, source_id) DO UPDATE SET is_active = true, updated_at = now()
             RETURNING id",
        )
        .bind(&source_id)
        .bind(&title)
        .bind(&summary)
        .bind(&door.grantor_name)
        .bind(&door.entry_url)
        .bind(&condition)
        .bind(row.ends_on.and_then(end_of_kst_day))
        .bind(json!({"display": "verbatim", "door": {"door_id": door.id, "offer_key": row.offer_key, "tiers": row.tiers}}))
        .bind(json!({"door_offer": {"quote": row.quote, "restriction": row.restriction, "value_kind": row.value_kind,
                                    "value": row.value, "upper_bound": row.upper_bound}}))
        .fetch_one(&mut **tx)
        .await?,
    };
    sqlx::query(
        "INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
         VALUES ($1, $2, 'sponsor', true), ($1, $3, 'redeemable_at', false)
         ON CONFLICT DO NOTHING",
    )
    .bind(program)
    .bind(door.grantor_brand_id)
    .bind(row.redeem_brand_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at,
                                              review_due_at, recheck_interval_seconds, verification_mode)
         VALUES ($1, $2, 'current', now(), now() + interval '3 days', 86400, 'door')
         ON CONFLICT (program_id, candidate_id) DO UPDATE SET verification_state = 'current', verified_at = now(),
                                                             review_due_at = now() + interval '3 days', updated_at = now()",
    )
    .bind(program)
    .bind(candidate)
    .execute(&mut **tx)
    .await?;
    Ok(program)
}

/// Claims the one identity of a shown corporate benefit, (issuer, redeem
/// brand, value kind), for `program` in the publishing transaction (code
/// review H-3). False when another programme holds it: the caller does not
/// publish. Intake (`create_programme`) claims through the same table.
pub(crate) async fn claim_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: Uuid,
    redeem: Uuid,
    value_kind: &str,
    program: Uuid,
) -> Result<bool> {
    let claimed = sqlx::query(
        "INSERT INTO corporate_benefit_identities (issuer_brand_id, redeem_brand_id, value_kind, program_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (issuer_brand_id, redeem_brand_id, value_kind) DO UPDATE
             SET program_id = EXCLUDED.program_id
             WHERE corporate_benefit_identities.program_id = EXCLUDED.program_id",
    )
    .bind(issuer)
    .bind(redeem)
    .bind(value_kind)
    .bind(program)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(claimed == 1)
}

/// The hide sweep (code review C-1, security review M4, H5): runs on every
/// worker tick whatever the door switches say, reads the database only and
/// makes no request. A shown hub offer is hidden when
/// - its `door` monitor is past `review_due_at` (only a live confirming
///   read moves it, 3 days ahead), so turning the mode off, removing the
///   kind or taking intake out of live hides every hub offer within 3 days;
/// - its row is missing from the last complete read, or its end date passed;
/// - a display opt-out covers the hub, the grantor or the redeem brand.
///
/// A door page's programme published through intake whose sponsor or
/// redeem brand has opted out is held (`publisher_optout`). Returns how
/// many it hid or held.
pub async fn sweep(pool: &PgPool, today: NaiveDate) -> Result<usize> {
    let rows: Vec<(Uuid, Option<Uuid>, Uuid, Uuid, Uuid, bool)> = sqlx::query_as(
        "SELECT o.id, o.program_id, dr.domain_id, o.redeem_brand_id, dr.grantor_brand_id,
                (o.missing_since IS NOT NULL
                 OR (o.ends_on IS NOT NULL AND o.ends_on < $1)
                 OR NOT EXISTS (SELECT 1 FROM program_source_monitors m
                                WHERE m.program_id = o.program_id AND m.verification_mode = 'door'
                                  AND m.verification_state = 'current' AND m.review_due_at > now()))
         FROM corporate_door_offers o JOIN corporate_doors dr ON dr.id = o.door_id
         WHERE o.status = 'published'",
    )
    .bind(today)
    .fetch_all(pool)
    .await?;
    let mut changed = 0;
    for (id, program, domain, redeem, grantor, stale) in rows {
        let mut tx = pool.begin().await?;
        let opted_out: bool = sqlx::query_scalar(OPTED_OUT_SQL)
            .bind(domain)
            .bind(redeem)
            .bind(grantor)
            .fetch_one(&mut *tx)
            .await?;
        if stale || opted_out {
            hide(&mut tx, id, program).await?;
            changed += 1;
        }
        tx.commit().await?;
    }
    // Door pages published through intake: an opt-out of the issuer (the
    // sponsor) or of the brand holds them.
    let held = sqlx::query(
        "INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active,
                                           previous_program_status)
         SELECT DISTINCT p.id, m.candidate_id, 'publisher_optout', p.is_active, p.program_status
         FROM programs p
         JOIN program_source_monitors m ON m.program_id = p.id AND m.verification_state <> 'retired'
         JOIN corporate_doors dr ON dr.candidate_id = m.candidate_id AND dr.kind <> 'hub_rows'
         JOIN benefit_source_domains d ON d.id = dr.domain_id
         WHERE p.is_active
           AND EXISTS (
               SELECT 1 FROM program_brands pb, benefit_source_optouts o
               WHERE pb.program_id = p.id
                 AND o.lifted_at IS NULL AND o.effective_at IS NOT NULL AND o.scope IN ('display', 'both')
                 AND (d.id = ANY(o.domain_ids)
                      OR EXISTS (SELECT 1 FROM benefit_source_domain_brands db
                                 WHERE db.brand_id = pb.brand_id AND db.domain_id = ANY(o.domain_ids))
                      OR EXISTS (SELECT 1 FROM brand_discovery_targets t
                                 WHERE t.brand_id = pb.brand_id AND t.official_domain IS NOT NULL
                                   AND o.domain IS NOT NULL
                                   AND regexp_replace(lower(t.official_domain), '^www\\.', '')
                                       = regexp_replace(lower(o.domain), '^www\\.', ''))))
           AND NOT EXISTS (SELECT 1 FROM program_source_holds h
                           WHERE h.program_id = p.id AND h.resolved_at IS NULL AND h.hold_reason = 'publisher_optout')
         RETURNING program_id",
    )
    .execute(pool)
    .await?
    .rows_affected();
    if held > 0 {
        sqlx::query(
            "UPDATE programs p SET is_active = false, updated_at = now()
             WHERE p.is_active AND EXISTS (SELECT 1 FROM program_source_holds h WHERE h.program_id = p.id
                                           AND h.resolved_at IS NULL AND h.hold_reason = 'publisher_optout')",
        )
        .execute(pool)
        .await?;
    }
    // A hub programme hidden for 30 days is retired (code review M-15): a
    // changed value is a new programme, so the old one would otherwise sit
    // hidden for ever. Retirement keeps the row (saved items still point at
    // it); the offer's return clears it.
    sqlx::query(
        "UPDATE programs p SET retired_at = now(), retirement_reason = 'provider_withdrawn', updated_at = now()
         WHERE NOT p.is_active AND p.retirement_reason IS NULL AND p.updated_at < now() - interval '30 days'
           AND EXISTS (SELECT 1 FROM corporate_door_offers o WHERE o.program_id = p.id AND o.status = 'gone')",
    )
    .execute(pool)
    .await?;
    Ok(changed + held as usize)
}

/// Retires doors lost [`RETIRE_AFTER_DAYS`] days with no replacement
/// (`lost_no_replacement_30d`, a counted outcome). Returns how many.
pub async fn retire_unreplaced(pool: &PgPool) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE corporate_doors SET state = 'retired', last_outcome = 'lost_no_replacement_30d', updated_at = now()
         WHERE state = 'lost' AND replaced_by IS NULL AND lost_since < now() - make_interval(days => $1)",
    )
    .bind(RETIRE_AFTER_DAYS as i32)
    .execute(pool)
    .await?
    .rows_affected())
}

/// How close two URLs are by path: shared leading segments, then shared
/// segments anywhere.
pub fn path_closeness(lost: &Url, candidate: &Url) -> (usize, usize) {
    let segments = |u: &Url| -> Vec<String> {
        u.path()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect()
    };
    let (a, b) = (segments(lost), segments(candidate));
    let leading = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let shared = a.iter().filter(|s| b.contains(s)).count();
    (leading, shared)
}

/// Whether `found` (a read of a replacement page) is the same door as
/// `door` was: a hub sharing at least half its last brands (and five), a
/// page stating one of its offers, a list of links.
pub fn same_door(door: &DoorRow, found: &job::DoorRead, known_offers: &BTreeSet<String>) -> bool {
    match door.kind() {
        Some(Kind::HubRows) => {
            let last: BTreeSet<Uuid> = door.last_matched_brands.iter().copied().collect();
            let overlap = found.matched_brands.intersection(&last).count();
            !last.is_empty() && overlap >= 5 && overlap * 2 >= last.len()
        }
        Some(Kind::Page) => found
            .offers
            .iter()
            .any(|o| known_offers.contains(&o.offer_key)),
        Some(Kind::Listing) => found.verdict == "listing",
        None => false,
    }
}

/// One pass over lost doors (see the module docs). Returns replacements.
pub async fn rediscover(
    pool: &PgPool,
    flags: &DoorFlags,
    http: &dyn PageHttp,
    test_spacing: Option<std::time::Duration>,
    only: Option<&[Uuid]>,
) -> Result<usize> {
    if !flags.reads() {
        return Ok(0);
    }
    let registry = std::sync::Arc::new(Registry::load(pool).await?);
    let today = crate::corporate_auto::health::kst_today();
    let lost: Vec<DoorRow> = sqlx::query_as::<_, DoorRow>(&format!(
        "SELECT dr.id, dr.domain_id, d.domain, dr.entry_url, dr.candidate_id, dr.grantor_brand_id,
                b.name AS grantor_name, dr.kind, dr.kind AS lane, dr.parent_door_id, dr.max_bytes, dr.state,
                dr.first_gone_at,
                dr.unrecognised_days, dr.consecutive_failures, dr.last_matched_brands, d.gray_robots_missing,
                d.gray_crawl_delay_min, d.min_request_interval_ms
         FROM corporate_doors dr JOIN benefit_source_domains d ON d.id = dr.domain_id
         JOIN brands b ON b.id = dr.grantor_brand_id
         WHERE dr.state = 'lost' AND dr.replaced_by IS NULL AND dr.parent_door_id IS NULL
           -- A door the site closed by robots is not looked for again
           -- (security review L2).
           AND COALESCE(dr.last_outcome, '') <> 'robots_disallow'
           AND {policy} AND (dr.kind <> 'hub_rows' OR {hub})
           AND (dr.flag IS NULL OR dr.flag = ANY($1))
           AND ($3::uuid[] IS NULL OR dr.id = ANY($3))
           AND NOT EXISTS (SELECT 1 FROM corporate_door_reads r WHERE r.door_id = dr.id
                           AND r.verdict = 'rediscovery' AND r.read_at > now() - interval '1 day')
         ORDER BY dr.lost_since LIMIT $2",
        policy = crate::corporate_auto::policy::DOMAIN_POLICY_SQL,
        hub = super::HUB_TERMS_ALLOWED_SQL,
    ))
    .bind(&flags.flags_on)
    .bind(REDISCOVERY_DOORS_PER_RUN)
    .bind(only)
    .fetch_all(pool)
    .await?;
    let optouts = LiveOptOuts(pool);
    let mut replaced = 0;
    for door in lost {
        let Ok(lost_url) = Url::parse(&door.entry_url) else {
            continue;
        };
        let mut session = DomainSession::new(
            http,
            test_spacing.is_none().then_some(pool),
            test_spacing.is_none().then_some(door.domain_id),
            door.gray_crawl_delay_min,
            door.min_request_interval_ms as i64,
            job::MAX_REQUESTS_PER_DOMAIN,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        let known: BTreeSet<String> =
            sqlx::query_scalar("SELECT offer_key FROM corporate_door_offers WHERE door_id = $1")
                .bind(door.id)
                .fetch_all(pool)
                .await?
                .into_iter()
                .collect();
        // Never a URL that is a door already, nor one an earlier pass of
        // this door probed (security review L2): each pass tries the next
        // closest URLs instead of the same three for 30 days.
        let skip: BTreeSet<String> = sqlx::query_scalar(
            "SELECT entry_url FROM corporate_doors WHERE domain_id = $1
             UNION
             SELECT jsonb_array_elements_text(r.by_reason->'probed') FROM corporate_door_reads r
             WHERE r.door_id = $2 AND r.verdict = 'rediscovery'",
        )
        .bind(door.domain_id)
        .bind(door.id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect();
        let mut candidates: Vec<Url> = crate::corporate_auto::evidence::sitemap_urls(
            &mut session,
            &lost_url,
            door.gray_robots_missing,
        )
        .await
        .into_iter()
        .filter(|u| u.host_str() == lost_url.host_str() && *u != lost_url)
        .filter(|u| u.as_str().len() <= 1000 && !skip.contains(u.as_str()))
        .collect();
        candidates.sort_by_key(|u| std::cmp::Reverse(path_closeness(&lost_url, u)));
        let mut found = None;
        let mut probed: Vec<String> = Vec::new();
        for url in candidates.into_iter().take(REDISCOVERY_FETCHES) {
            probed.push(url.to_string());
            let probe = DoorRow {
                entry_url: url.to_string(),
                ..door.clone()
            };
            if let Fetch::Html(html, page_url) =
                job::fetch_door(&mut session, &probe, Some(&optouts)).await
            {
                let (registry, door_probe, read_url) =
                    (registry.clone(), probe.clone(), page_url.clone());
                let Ok(read) = tokio::task::spawn_blocking(move || {
                    job::interpret(&door_probe, &html, &read_url, &registry, today)
                })
                .await
                else {
                    continue;
                };
                if same_door(&door, &read, &known) {
                    found = Some(page_url);
                    break;
                }
            }
        }
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO corporate_door_reads (door_id, mode, verdict, requests, by_reason)
             VALUES ($1, 'shadow', 'rediscovery', $2, $3)",
        )
        .bind(door.id)
        .bind(session.requests() as i32)
        .bind(serde_json::json!({ "probed": probed }))
        .execute(&mut *tx)
        .await?;
        if let Some(url) = found {
            let new: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind, flag, max_bytes,
                                              last_matched_brands)
                 SELECT domain_id, $2, grantor_brand_id, kind, flag, max_bytes, last_matched_brands
                 FROM corporate_doors WHERE id = $1
                 ON CONFLICT (entry_url) DO NOTHING RETURNING id",
            )
            .bind(door.id)
            .bind(url.as_str())
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(new) = new {
                sqlx::query(
                    "UPDATE corporate_doors SET state = 'retired', replaced_by = $2, last_outcome = 'replaced',
                                                updated_at = now() WHERE id = $1",
                )
                .bind(door.id)
                .bind(new)
                .execute(&mut *tx)
                .await?;
                crate::operations::resolve_event_tx(
                    &mut tx,
                    &format!("corporate_door_lost:{}", door.id),
                    None,
                    Some("replacement found on the same host"),
                )
                .await?;
                replaced += 1;
            }
        }
        tx.commit().await?;
    }
    Ok(replaced)
}
