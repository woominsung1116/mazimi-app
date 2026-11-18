//! Daily self-metrics of the resolver and the degraded signal.
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};

/// A fall of the application+notice share by more than this many
/// percentage points from the previous day degrades the source.
pub const MAX_SHARE_DROP_PP: f64 = 2.0;
/// More than this share of a source's resolved programmes showing a link
/// whose last check was a hard failure degrades the source (timeouts from
/// the data centre are not counted).
pub const MAX_FAILING_SHARE_PCT: f64 = 1.0;

#[derive(Debug, Clone, Default, FromRow, Serialize, PartialEq)]
pub struct Counts {
    pub source_type: String,
    pub programmes: i64,
    pub application: i64,
    pub notice: i64,
    pub information: i64,
    pub portal: i64,
    pub failing: i64,
    pub no_link: i64,
    pub pending: i64,
    pub candidates: i64,
    pub broken_candidates: i64,
}

impl Counts {
    /// Programmes the resolver has decided (pending ones are not counted:
    /// new programmes waiting for a first check are not a regression).
    pub fn resolved(&self) -> i64 {
        self.programmes - self.pending
    }
    pub fn share_ab(&self) -> f64 {
        if self.resolved() <= 0 {
            return 0.0;
        }
        100.0 * (self.application + self.notice) as f64 / self.resolved() as f64
    }
    pub fn failing_pct(&self) -> f64 {
        if self.resolved() <= 0 {
            return 0.0;
        }
        100.0 * self.failing as f64 / self.resolved() as f64
    }
}

/// Why `today` is degraded against `yesterday`, if it is.
pub fn degraded_reasons(today: &Counts, yesterday: Option<&Counts>) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(prev) = yesterday {
        let drop = prev.share_ab() - today.share_ab();
        if drop > MAX_SHARE_DROP_PP {
            reasons.push(format!(
                "{}: application+notice share fell {:.1}pp ({:.1}% → {:.1}%)",
                today.source_type,
                drop,
                prev.share_ab(),
                today.share_ab()
            ));
        }
    }
    if today.failing_pct() > MAX_FAILING_SHARE_PCT {
        reasons.push(format!(
            "{}: {:.1}% of programmes show a link whose last check failed",
            today.source_type,
            today.failing_pct()
        ));
    }
    reasons
}

pub async fn current(pool: &PgPool) -> Result<Vec<Counts>, sqlx::Error> {
    sqlx::query_as(
        "WITH sel AS (
           SELECT p.source_type, s.state, c.kind, c.check_status
           FROM current_programs p
           LEFT JOIN program_link_selections s ON s.program_id=p.id
           LEFT JOIN program_link_candidates c ON c.id=s.selected_id),
         cand AS (
           SELECT p.source_type, count(*) AS candidates, count(*) FILTER (WHERE c.check_status='broken') AS broken
           FROM program_link_candidates c JOIN current_programs p ON p.id=c.program_id GROUP BY 1)
         SELECT sel.source_type,
           count(*) AS programmes,
           count(*) FILTER (WHERE state='selected' AND kind='application') AS application,
           count(*) FILTER (WHERE state='selected' AND kind='notice') AS notice,
           count(*) FILTER (WHERE state='selected' AND kind='information') AS information,
           count(*) FILTER (WHERE state='selected' AND kind='portal') AS portal,
           count(*) FILTER (WHERE state='selected' AND check_status='hard_fail') AS failing,
           count(*) FILTER (WHERE state='none') AS no_link,
           count(*) FILTER (WHERE state IS NULL OR state='pending' OR (state='selected' AND kind IS NULL)) AS pending,
           COALESCE(max(cand.candidates),0)::bigint AS candidates,
           COALESCE(max(cand.broken),0)::bigint AS broken_candidates
         FROM sel LEFT JOIN cand ON cand.source_type=sel.source_type
         GROUP BY sel.source_type ORDER BY sel.source_type",
    )
    .fetch_all(pool)
    .await
}

/// Stores today's counts (Korean day), compares them with the previous
/// recorded day and returns the daily job details (`degraded` when a
/// threshold is crossed). Each degraded source opens a warning event that
/// resolves itself on a healthy day.
pub async fn record(pool: &PgPool) -> anyhow::Result<Value> {
    use crate::operations::{record_event, resolve_event, EventCommand, Severity};
    let today = current(pool).await?;
    let mut reasons = Vec::new();
    for counts in &today {
        let previous: Option<Counts> = sqlx::query_as(
            "SELECT source_type,programmes,application,notice,information,portal,failing,no_link,pending,candidates,broken_candidates
             FROM link_resolver_metrics
             WHERE source_type=$1 AND day < (now() AT TIME ZONE 'Asia/Seoul')::date
             ORDER BY day DESC LIMIT 1",
        )
        .bind(&counts.source_type)
        .fetch_optional(pool)
        .await?;
        sqlx::query(
            "INSERT INTO link_resolver_metrics(day,source_type,programmes,application,notice,information,portal,failing,no_link,pending,candidates,broken_candidates)
             VALUES((now() AT TIME ZONE 'Asia/Seoul')::date,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
             ON CONFLICT(day,source_type) DO UPDATE SET programmes=excluded.programmes,application=excluded.application,
               notice=excluded.notice,information=excluded.information,portal=excluded.portal,failing=excluded.failing,
               no_link=excluded.no_link,pending=excluded.pending,candidates=excluded.candidates,
               broken_candidates=excluded.broken_candidates,recorded_at=now()",
        )
        .bind(&counts.source_type)
        .bind(counts.programmes)
        .bind(counts.application)
        .bind(counts.notice)
        .bind(counts.information)
        .bind(counts.portal)
        .bind(counts.failing)
        .bind(counts.no_link)
        .bind(counts.pending)
        .bind(counts.candidates)
        .bind(counts.broken_candidates)
        .execute(pool)
        .await?;
        let found = degraded_reasons(counts, previous.as_ref());
        let fingerprint = format!("link_resolver:degraded:{}", counts.source_type);
        if found.is_empty() {
            resolve_event(
                pool,
                &fingerprint,
                None,
                Some("link metrics back within thresholds"),
            )
            .await?;
        } else {
            record_event(
                pool,
                &EventCommand {
                    fingerprint: &fingerprint,
                    severity: Severity::Warning,
                    component: "link_resolver",
                    event_type: "link_resolver_degraded",
                    subject_type: Some("source"),
                    subject_id: Some(&counts.source_type),
                    message: "Fewer programmes show an application or notice link than yesterday, or too many show a link that failed its last check",
                    details: json!({"reasons": found, "today": counts, "previous": previous}),
                },
            )
            .await?;
            reasons.extend(found);
        }
    }
    let mut details = json!({"sources": today});
    if !reasons.is_empty() {
        details["degraded"] = json!(true);
        details["reason"] = json!(reasons.join("; "));
    }
    Ok(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(application: i64, notice: i64, failing: i64) -> Counts {
        Counts {
            source_type: "youth_center".into(),
            programmes: 1000,
            application,
            notice,
            failing,
            ..Counts::default()
        }
    }

    #[test]
    fn a_share_drop_over_two_points_or_failing_links_over_one_percent_degrade() {
        let yesterday = counts(30, 570, 0);
        assert!(degraded_reasons(&counts(30, 550, 0), Some(&yesterday)).is_empty());
        assert_eq!(
            degraded_reasons(&counts(30, 549, 0), Some(&yesterday)).len(),
            1
        );
        assert!(degraded_reasons(&counts(30, 570, 10), Some(&yesterday)).is_empty());
        assert_eq!(degraded_reasons(&counts(30, 570, 11), None).len(), 1);
        // The first day has nothing to compare with.
        assert!(degraded_reasons(&counts(0, 0, 0), None).is_empty());
        // 300 new programmes waiting for a first check are not a drop.
        let mut grown = counts(30, 570, 0);
        grown.programmes += 300;
        grown.pending = 300;
        assert!(degraded_reasons(&grown, Some(&yesterday)).is_empty());
    }
}
