//! The door numbers: the shadow gate metrics of review H7, printed by
//! `corporate-doors metrics`, and the daily health block.
//!
//! Gates (what "ready for live" is measured by, per lane):
//! 1. **source agreement**: offers of the same (issuer, redeem brand, value
//!    kind) stated by two different doors (a hub row and the brand's own
//!    page) must agree; zero contradictions over at least
//!    [`MIN_AGREEMENT_PAIRS`] pairs.
//! 2. **identical re-read**: of the accepted offers re-read in the window,
//!    at least [`MIN_IDENTICAL_SHARE`] read the same quote every time.
//! 3. **rejection reasons**: what the readers refused, by reason.
//! 4. **must-have brands accepted per lane**.
//! 5. **ground truth**: [`GROUND_TRUTH_ROWS`] accepted rows per lane
//!    checked against the live page with zero errors; who checks is the
//!    owner's decision, so the number of rows available is printed, never a
//!    model's verdict.

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::BTreeMap;

pub const MIN_AGREEMENT_PAIRS: i64 = 20;
pub const MIN_IDENTICAL_SHARE: f64 = 0.95;
pub const GROUND_TRUTH_ROWS: i64 = 150;
/// Must-have brands visible may fall this much (share) in a day, after
/// discounting brands whose offers ended by date, before an error.
pub const VISIBLE_BRANDS_DROP: f64 = 0.10;
/// The drop alert needs at least this many visible must-have brands the day
/// before.
pub const MIN_VISIBLE_BRANDS_FOR_ALERT: f64 = 20.0;

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Lane {
    pub reads: i64,
    pub verdicts: BTreeMap<String, i64>,
    pub accepted_offers: i64,
    pub must_have_brands: Vec<String>,
    pub tiered_rows: i64,
    pub rows_matched: i64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct GateMetrics {
    pub window_days: i32,
    pub lanes: BTreeMap<String, Lane>,
    pub rejection_reasons: BTreeMap<String, i64>,
    pub reread_offers: i64,
    pub reread_identical: i64,
    pub agreement_pairs: i64,
    pub contradictions: Vec<String>,
    pub doors_by_state: BTreeMap<String, i64>,
    pub lost_no_replacement: i64,
    pub published: i64,
}

impl GateMetrics {
    pub fn identical_share(&self) -> Option<f64> {
        (self.reread_offers > 0).then(|| self.reread_identical as f64 / self.reread_offers as f64)
    }

    /// Gates 1 and 2 (the automatic ones).
    pub fn automatic_gates_pass(&self) -> bool {
        self.agreement_pairs >= MIN_AGREEMENT_PAIRS
            && self.contradictions.is_empty()
            && self
                .identical_share()
                .is_some_and(|s| s >= MIN_IDENTICAL_SHARE)
    }
}

/// The lane of a door: its own kind, or its parent's for a page a list
/// found.
const LANE_SQL: &str = "COALESCE(parent.kind, dr.kind)";

pub async fn collect(pool: &PgPool, days: i32) -> Result<GateMetrics> {
    let mut m = GateMetrics {
        window_days: days,
        ..GateMetrics::default()
    };
    let reads: Vec<(String, String, i64, i64, i64)> = sqlx::query_as(&format!(
        "SELECT {LANE_SQL}, r.verdict, count(*), COALESCE(sum(r.tiered_rows), 0)::bigint,
                COALESCE(sum(r.rows_matched), 0)::bigint
         FROM corporate_door_reads r JOIN corporate_doors dr ON dr.id = r.door_id
         LEFT JOIN corporate_doors parent ON parent.id = dr.parent_door_id
         WHERE r.read_at > now() - make_interval(days => $1) AND r.verdict <> 'rediscovery'
         GROUP BY 1, 2"
    ))
    .bind(days)
    .fetch_all(pool)
    .await?;
    for (lane, verdict, n, tiered, matched) in reads {
        let entry = m.lanes.entry(lane).or_default();
        entry.reads += n;
        entry.tiered_rows += tiered;
        entry.rows_matched += matched;
        *entry.verdicts.entry(verdict).or_default() += n;
    }
    let reasons: Vec<(String, i64)> = sqlx::query_as(
        "SELECT e.key, sum(e.value::bigint)::bigint
         FROM corporate_door_reads r, jsonb_each_text(r.by_reason) e
         WHERE r.read_at > now() - make_interval(days => $1)
         GROUP BY e.key ORDER BY e.key",
    )
    .bind(days)
    .fetch_all(pool)
    .await?;
    m.rejection_reasons = reasons.into_iter().collect();
    let accepted: Vec<(String, i64, Vec<String>)> = sqlx::query_as(&format!(
        "SELECT {LANE_SQL}, count(DISTINCT o.id),
                COALESCE(array_agg(DISTINCT b.name ORDER BY b.name) FILTER (WHERE t.must_have), '{{}}')
         FROM corporate_door_offers o JOIN corporate_doors dr ON dr.id = o.door_id
         LEFT JOIN corporate_doors parent ON parent.id = dr.parent_door_id
         JOIN brands b ON b.id = o.redeem_brand_id
         LEFT JOIN brand_discovery_targets t ON t.brand_id = o.redeem_brand_id AND t.must_have
         WHERE o.last_seen_at > now() - make_interval(days => $1) AND o.status <> 'gone'
         GROUP BY 1"
    ))
    .bind(days)
    .fetch_all(pool)
    .await?;
    for (lane, n, brands) in accepted {
        let entry = m.lanes.entry(lane).or_default();
        entry.accepted_offers = n;
        entry.must_have_brands = brands;
    }
    let (reread, identical): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE rereads > 0), count(*) FILTER (WHERE rereads > 0 AND rereads_identical = rereads)
         FROM corporate_door_offers WHERE last_seen_at > now() - make_interval(days => $1)",
    )
    .bind(days)
    .fetch_one(pool)
    .await?;
    m.reread_offers = reread;
    m.reread_identical = identical;
    let pairs: Vec<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT ib.name, rb.name, a.value || CASE WHEN a.upper_bound THEN ' (max)' ELSE '' END,
                b.value || CASE WHEN b.upper_bound THEN ' (max)' ELSE '' END,
                a.value = b.value AND a.upper_bound = b.upper_bound
         FROM corporate_door_offers a
         JOIN corporate_door_offers b ON b.issuer_brand_id = a.issuer_brand_id
              AND b.redeem_brand_id = a.redeem_brand_id AND b.value_kind = a.value_kind AND b.door_id > a.door_id
         JOIN brands ib ON ib.id = a.issuer_brand_id JOIN brands rb ON rb.id = a.redeem_brand_id
         WHERE a.status <> 'gone' AND b.status <> 'gone'
           AND a.last_seen_at > now() - make_interval(days => $1)
           AND b.last_seen_at > now() - make_interval(days => $1)",
    )
    .bind(days)
    .fetch_all(pool)
    .await?;
    m.agreement_pairs = pairs.len() as i64;
    m.contradictions = pairs
        .into_iter()
        .filter(|p| !p.4)
        .map(|(issuer, redeem, a, b, _)| format!("{issuer} · {redeem}: {a} vs {b}"))
        .collect();
    let states: Vec<(String, i64)> =
        sqlx::query_as("SELECT state, count(*) FROM corporate_doors GROUP BY state ORDER BY state")
            .fetch_all(pool)
            .await?;
    m.doors_by_state = states.into_iter().collect();
    m.lost_no_replacement = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_doors WHERE last_outcome = 'lost_no_replacement_30d'",
    )
    .fetch_one(pool)
    .await?;
    m.published =
        sqlx::query_scalar("SELECT count(*) FROM corporate_door_offers WHERE status = 'published'")
            .fetch_one(pool)
            .await?;
    Ok(m)
}

/// The text `corporate-doors metrics` prints.
pub fn render(m: &GateMetrics) -> String {
    let mut out = String::new();
    let push = |out: &mut String, line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    push(
        &mut out,
        format!("corporate doors, last {} days", m.window_days),
    );
    push(&mut out, format!("doors by state: {:?}", m.doors_by_state));
    push(
        &mut out,
        format!("lost, no replacement in 30 days: {}", m.lost_no_replacement),
    );
    for (lane, l) in &m.lanes {
        push(
            &mut out,
            format!(
                "lane {lane}: reads {} verdicts {:?} accepted offers {} (ground-truth sample frame; {} rows needed) tiered rows {} of {} matched",
                l.reads, l.verdicts, l.accepted_offers, GROUND_TRUTH_ROWS, l.tiered_rows, l.rows_matched
            ),
        );
        push(
            &mut out,
            format!(
                "  must-have brands accepted: {} [{}]",
                l.must_have_brands.len(),
                l.must_have_brands.join(", ")
            ),
        );
    }
    push(
        &mut out,
        format!("rejection reasons: {:?}", m.rejection_reasons),
    );
    push(
        &mut out,
        format!(
            "identical re-reads: {} of {} ({}; gate ≥ {:.0}%)",
            m.reread_identical,
            m.reread_offers,
            m.identical_share()
                .map_or("n/a".to_string(), |s| format!("{:.1}%", s * 100.0)),
            MIN_IDENTICAL_SHARE * 100.0
        ),
    );
    push(
        &mut out,
        format!(
            "source agreement: {} pairs, {} contradictions (gate ≥ {MIN_AGREEMENT_PAIRS} pairs, 0 contradictions)",
            m.agreement_pairs,
            m.contradictions.len()
        ),
    );
    for c in &m.contradictions {
        push(&mut out, format!("  contradiction: {c}"));
    }
    push(&mut out, format!("published hub offers: {}", m.published));
    push(
        &mut out,
        format!(
            "automatic gates: {}",
            if m.automatic_gates_pass() {
                "pass"
            } else {
                "not yet"
            }
        ),
    );
    out
}

/// The daily health block (`health::collect`, key `doors`).
pub async fn health_block(pool: &PgPool) -> Result<Value> {
    let states: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT kind, state, count(*) FROM corporate_doors GROUP BY 1, 2 ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await?;
    let (lost_over_7d, last_success_age_p95_h): (i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE state = 'lost' AND lost_since < now() - interval '7 days'),
                percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM now() - last_success_at) / 3600)
                    FILTER (WHERE state = 'active' AND last_success_at IS NOT NULL)
         FROM corporate_doors",
    )
    .fetch_one(pool)
    .await?;
    let (must_have, visible, ended_by_date, shadow_would_show): (i64, i64, i64, i64) =
        sqlx::query_as(
            "WITH targets AS (SELECT DISTINCT brand_id FROM brand_discovery_targets
                          WHERE must_have AND is_active AND brand_id IS NOT NULL),
              -- Where the benefit is used: a sponsor that pays (an issuer)
              -- is not a visible brand of its own (code review M-9).
              shown AS (SELECT DISTINCT pb.brand_id FROM current_programs p
                        JOIN program_brands pb ON pb.program_id = p.id AND pb.relationship_type = 'redeemable_at'
                        WHERE p.program_type = 'corporate_benefit'),
              -- Ended by its stated date in the last day, whether or not a
              -- job has hidden it since (M-9).
              ended AS (SELECT DISTINCT pb.brand_id FROM programs p
                        JOIN program_brands pb ON pb.program_id = p.id AND pb.relationship_type = 'redeemable_at'
                        WHERE p.program_type = 'corporate_benefit'
                          AND p.benefit_valid_until BETWEEN now() - interval '1 day' AND now())
         SELECT (SELECT count(*) FROM targets),
                (SELECT count(*) FROM targets t JOIN shown s USING (brand_id)),
                (SELECT count(*) FROM targets t JOIN ended e USING (brand_id)
                  WHERE NOT EXISTS (SELECT 1 FROM shown s WHERE s.brand_id = t.brand_id)),
                (SELECT count(DISTINCT o.redeem_brand_id) FROM corporate_door_offers o
                   JOIN targets t ON t.brand_id = o.redeem_brand_id
                  WHERE o.status <> 'gone' AND o.missing_since IS NULL
                    AND o.last_seen_at > now() - interval '2 days')",
        )
        .fetch_one(pool)
        .await?;
    let reasons: Vec<(String, i64)> = sqlx::query_as(
        "SELECT e.key, sum(e.value::bigint)::bigint FROM corporate_door_reads r, jsonb_each_text(r.by_reason) e
         WHERE r.read_at > now() - interval '1 day' GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    let visit_window: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_door_reads WHERE verdict = 'visit_window' AND read_at > now() - interval '1 day'",
    )
    .fetch_one(pool)
    .await?;
    let mut by_state: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (kind, state, n) in states {
        by_state.entry(kind).or_default().insert(state, n);
    }
    let mode = super::DoorFlags::from_env().map_or("invalid", |f| f.mode.as_str());
    Ok(json!({
        "mode": mode,
        "by_kind_state": by_state,
        "lost_over_7d": lost_over_7d,
        "last_success_age_p95_h": last_success_age_p95_h,
        "rows_rejected_by_reason_24h": reasons.into_iter().collect::<BTreeMap<_, _>>(),
        "visit_window_deferrals_24h": visit_window,
        "must_have": {"total": must_have, "visible_brands": visible,
                      "ended_by_date_24h": ended_by_date,
                      "shadow_would_show_brands": shadow_would_show},
    }))
}

/// The must-have visible-brands alert (review H8): a fall of more than
/// [`VISIBLE_BRANDS_DROP`] day over day, after the brands whose offers
/// simply ended by their stated date are discounted, so the monthly expiry
/// of hub promotions never fires it. Pure.
pub fn visible_brands_drop(today: &Value, previous: Option<&Value>) -> Option<f64> {
    // Only while doors run (code review "live at merge"): with the mode off
    // the existing offers' own alerts cover them.
    if !matches!(today["doors"]["mode"].as_str(), Some("shadow" | "live")) {
        return None;
    }
    let now = today["doors"]["must_have"]["visible_brands"].as_f64()?;
    let ended = today["doors"]["must_have"]["ended_by_date_24h"]
        .as_f64()
        .unwrap_or(0.0);
    let before = previous?["doors"]["must_have"]["visible_brands"].as_f64()?;
    // Below this many brands one held offer is a 10% move: no alert.
    if before < MIN_VISIBLE_BRANDS_FOR_ALERT {
        return None;
    }
    let unexplained = (before - ended - now) / before;
    (unexplained > VISIBLE_BRANDS_DROP).then_some(unexplained)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(visible: i64, ended: i64) -> Value {
        json!({"doors": {"mode": "shadow", "must_have": {"visible_brands": visible, "ended_by_date_24h": ended}}})
    }

    /// Live at merge with doors off: the alert never runs, and a small
    /// denominator never pages.
    #[test]
    fn the_drop_alert_needs_doors_running_and_a_denominator() {
        let mut off = day(60, 0);
        off["doors"]["mode"] = json!("off");
        assert_eq!(visible_brands_drop(&off, Some(&day(100, 0))), None);
        assert_eq!(
            visible_brands_drop(&day(8, 0), Some(&day(10, 0))),
            None,
            "10 brands: below the minimum"
        );
        assert!(visible_brands_drop(&day(16, 0), Some(&day(20, 0))).is_some());
    }

    /// Review H8: the first of the month, when 40 hub offers end by date,
    /// is no alert; the same fall with no date explaining it is.
    #[test]
    fn the_monthly_expiry_cliff_does_not_alert() {
        assert_eq!(visible_brands_drop(&day(60, 40), Some(&day(100, 0))), None);
        let drop = visible_brands_drop(&day(60, 0), Some(&day(100, 0))).unwrap();
        assert!((drop - 0.4).abs() < 1e-9);
        assert_eq!(visible_brands_drop(&day(95, 0), Some(&day(100, 0))), None);
        assert_eq!(visible_brands_drop(&day(95, 0), None), None);
    }

    #[test]
    fn the_automatic_gates_need_pairs_agreement_and_stable_rereads() {
        let mut m = GateMetrics {
            agreement_pairs: 25,
            reread_offers: 100,
            reread_identical: 96,
            ..GateMetrics::default()
        };
        assert!(m.automatic_gates_pass());
        m.contradictions.push("SK텔레콤 · CGV: 4000 vs 3000".into());
        assert!(!m.automatic_gates_pass());
        m.contradictions.clear();
        m.reread_identical = 90;
        assert!(!m.automatic_gates_pass());
        m.reread_identical = 96;
        m.agreement_pairs = 19;
        assert!(!m.automatic_gates_pass());
    }
}
