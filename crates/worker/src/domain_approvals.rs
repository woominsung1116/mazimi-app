//! Domain crawl approvals: automatic renewal on unchanged evidence, and
//! expiry notices 30 and 7 days ahead.
//!
//! Renewal is off by default (`DOMAIN_APPROVAL_AUTO_RENEW=true` turns it on)
//! and deliberately narrow:
//! - Only `public_facts_review` approvals whose terms review found no terms
//!   page (`not_found`) qualify. Every other basis depends on terms or on an
//!   external agreement, and no terms-page hash is stored to prove the terms
//!   are unchanged, so those always go to a person.
//! - A fresh server-side robots preflight must allow every approved path for
//!   our agent AND return byte-identical robots.txt (same hash as the stored
//!   evidence). A changed robots.txt goes to review, never to renewal.
//!
//! The renewal keeps the human-reviewed period, never exceeds 366 days from
//! now, and leaves its evidence in a `self_healing` operational event.
//! Anything not renewed expires as before (collection stops) and is listed
//! in the expiry notices.

use crate::operations::{self, EventCommand, Severity};
use crate::sources::benefit_pages::RobotsPreflightResult;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::future::Future;
use uuid::Uuid;

/// Bases that never depend on a terms page or an external agreement once the
/// terms review found no terms (`not_found`).
const RENEWABLE_BASES: &[&str] = &["public_facts_review"];
const MAX_RENEWALS_PER_RUN: i64 = 50;

#[derive(Debug, FromRow)]
struct Candidate {
    id: Uuid,
    domain: String,
    approval_basis: Option<String>,
    terms_review_outcome: Option<String>,
    allowed_path_prefixes: Vec<String>,
    crawler_contact_url: Option<String>,
    approval_expires_at: DateTime<Utc>,
    compliance_reviewed_at: Option<DateTime<Utc>>,
    approval_period_days: Option<i32>,
    approval_auto_renewed_at: Option<DateTime<Utc>>,
    robots_content_hash: Option<String>,
}

/// Why an approval may not be renewed by code, before any network check.
pub fn renewal_block(basis: Option<&str>, terms_outcome: Option<&str>) -> Option<&'static str> {
    match basis {
        Some(basis) if RENEWABLE_BASES.contains(&basis) => {}
        Some("written_permission" | "partner_contract") => {
            return Some("external_agreement_requires_review")
        }
        Some(_) => return Some("terms_dependent_requires_review"),
        None => return Some("no_approval_basis"),
    }
    match terms_outcome {
        Some("not_found") => None,
        None => Some("no_terms_classification"),
        Some(_) => Some("terms_dependent_requires_review"),
    }
}

/// The robots evidence is unchanged when the fresh document hashes to the
/// stored value.
fn robots_unchanged(stored: Option<&str>, fresh: &str) -> bool {
    stored.is_some_and(|stored| stored.eq_ignore_ascii_case(fresh))
}

/// The period to extend by: the stored one after an earlier automatic
/// renewal, else the span the human approved (expiry minus review time).
fn period_days(candidate: &Candidate) -> Option<i32> {
    let stored = candidate
        .approval_period_days
        .filter(|_| candidate.approval_auto_renewed_at > candidate.compliance_reviewed_at);
    let derived = candidate.compliance_reviewed_at.map(|reviewed| {
        let seconds = (candidate.approval_expires_at - reviewed).num_seconds();
        ((seconds + 86_399) / 86_400) as i32
    });
    stored
        .or(derived)
        .filter(|days| *days >= 1)
        .map(|days| days.min(366))
}

#[derive(Debug, Default, Serialize)]
pub struct ApprovalSummary {
    pub checked: usize,
    pub renewed: usize,
    /// domain -> reason, for approvals inside the renewal window that were
    /// left to expire.
    pub not_renewed: Vec<(String, &'static str)>,
    pub expiring_30d: i64,
    pub expiring_7d: i64,
}

/// `DOMAIN_APPROVAL_RENEW_WITHIN_DAYS` (default 30, 1..=60): how early a
/// renewal is attempted. Renewal runs only with
/// `DOMAIN_APPROVAL_AUTO_RENEW=true` (default off); the notices always run.
pub fn renewal_window_from_env() -> Result<Option<i32>> {
    let enabled = std::env::var("DOMAIN_APPROVAL_AUTO_RENEW").unwrap_or_default();
    if !matches!(
        enabled.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    ) {
        return Ok(None);
    }
    let days = match std::env::var("DOMAIN_APPROVAL_RENEW_WITHIN_DAYS") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<i32>()
            .context("DOMAIN_APPROVAL_RENEW_WITHIN_DAYS must be an integer")?,
        _ => 30,
    };
    anyhow::ensure!(
        (1..=60).contains(&days),
        "DOMAIN_APPROVAL_RENEW_WITHIN_DAYS must be between 1 and 60"
    );
    Ok(Some(days))
}

/// Renews what qualifies, then refreshes the 30- and 7-day notices.
/// `preflight(domain, paths, contact_url)` is the server-side robots check
/// (`benefit_pages::preflight_robots` in production).
pub async fn run<F, Fut>(
    pool: &PgPool,
    renew_within_days: Option<i32>,
    preflight: F,
) -> Result<ApprovalSummary>
where
    F: Fn(String, Vec<String>, String) -> Fut,
    Fut: Future<Output = Result<RobotsPreflightResult>>,
{
    let mut summary = ApprovalSummary::default();
    if let Some(days) = renew_within_days {
        let candidates = sqlx::query_as::<_, Candidate>(
            "SELECT id, domain, approval_basis, terms_review_outcome, allowed_path_prefixes,
                    crawler_contact_url, approval_expires_at, compliance_reviewed_at,
                    approval_period_days, approval_auto_renewed_at, robots_content_hash
             FROM benefit_source_domains
             WHERE is_active AND verification_status = 'verified' AND crawl_allowed
               AND compliance_status = 'approved'
               AND approval_expires_at > now()
               AND approval_expires_at <= now() + make_interval(days => $1)
             ORDER BY approval_expires_at, id LIMIT $2",
        )
        .bind(days)
        .bind(MAX_RENEWALS_PER_RUN)
        .fetch_all(pool)
        .await?;
        for candidate in candidates {
            summary.checked += 1;
            match renew_one(pool, &candidate, &preflight).await? {
                None => summary.renewed += 1,
                Some(reason) => {
                    tracing::warn!(
                        event = "domain_approval_not_renewed",
                        domain = %candidate.domain,
                        reason,
                        "domain approval left to expire"
                    );
                    summary.not_renewed.push((candidate.domain.clone(), reason));
                }
            }
        }
    }
    notices(pool, &mut summary).await?;
    Ok(summary)
}

async fn renew_one<F, Fut>(
    pool: &PgPool,
    candidate: &Candidate,
    preflight: &F,
) -> Result<Option<&'static str>>
where
    F: Fn(String, Vec<String>, String) -> Fut,
    Fut: Future<Output = Result<RobotsPreflightResult>>,
{
    if let Some(reason) = renewal_block(
        candidate.approval_basis.as_deref(),
        candidate.terms_review_outcome.as_deref(),
    ) {
        return Ok(Some(reason));
    }
    let Some(period) = period_days(candidate) else {
        return Ok(Some("no_review_period"));
    };
    let (Some(contact), false) = (
        candidate.crawler_contact_url.clone(),
        candidate.allowed_path_prefixes.is_empty(),
    ) else {
        return Ok(Some("no_path_scope"));
    };
    let robots = match preflight(
        candidate.domain.clone(),
        candidate.allowed_path_prefixes.clone(),
        contact,
    )
    .await
    {
        Ok(robots) => robots,
        Err(_) => return Ok(Some("robots_unavailable")),
    };
    if !robots.allowed {
        return Ok(Some("robots_disallows"));
    }
    if !robots_unchanged(
        candidate.robots_content_hash.as_deref(),
        &robots.content_hash,
    ) {
        return Ok(Some("robots_changed_requires_review"));
    }
    let renewed: Option<DateTime<Utc>> = sqlx::query_scalar(
        "UPDATE benefit_source_domains SET
             approval_period_days = $2,
             approval_expires_at = LEAST(approval_expires_at + make_interval(days => $2),
                                         now() + interval '366 days'),
             approval_auto_renewed_at = now(),
             approval_auto_renewals = approval_auto_renewals + 1,
             robots_url = $3, robots_checked_at = $4, robots_content_hash = $5,
             updated_at = now()
         WHERE id = $1 AND compliance_status = 'approved' AND crawl_allowed
           AND approval_expires_at = $6
         RETURNING approval_expires_at",
    )
    .bind(candidate.id)
    .bind(period)
    .bind(&robots.robots_url)
    .bind(robots.checked_at)
    .bind(robots.content_hash.to_ascii_lowercase())
    .bind(candidate.approval_expires_at)
    .fetch_optional(pool)
    .await?;
    let Some(new_expiry) = renewed else {
        // Changed concurrently (an admin re-approved or revoked it).
        return Ok(Some("changed_concurrently"));
    };
    let fingerprint = format!(
        "self_healing:domain_approval_renewed:{}:{}",
        candidate.id,
        new_expiry.format("%Y%m%d")
    );
    let id = candidate.id.to_string();
    operations::record_event(
        pool,
        &EventCommand {
            fingerprint: &fingerprint,
            severity: Severity::Info,
            component: "self_healing",
            event_type: "domain_approval_auto_renewed",
            subject_type: Some("benefit_source_domain"),
            subject_id: Some(&id),
            message: "robots.txt가 승인 때와 같은 내용이고 승인 경로를 모두 허용하며, 약관 페이지에 기대지 않는 승인이라 같은 기간만큼 자동 연장했습니다.",
            details: json!({
                "domain": candidate.domain,
                "automatic": true,
                "previous_expires_at": candidate.approval_expires_at,
                "new_expires_at": new_expiry,
                "period_days": period,
                "approval_basis": candidate.approval_basis,
                "terms_review_outcome": candidate.terms_review_outcome,
                "robots_url": robots.robots_url,
                "robots_checked_at": robots.checked_at,
                "robots_hash": robots.content_hash.to_ascii_lowercase(),
                "robots_unchanged": true,
                "terms": "not relied on (terms review found no terms page)",
                "paths": candidate.allowed_path_prefixes,
            }),
        },
    )
    .await?;
    operations::resolve_event(pool, &fingerprint, None, Some("automatic action completed")).await?;
    tracing::info!(
        event = "domain_approval_auto_renewed",
        domain = %candidate.domain,
        %new_expiry,
        period,
        "domain approval renewed on unchanged evidence"
    );
    Ok(None)
}

async fn notices(pool: &PgPool, summary: &mut ApprovalSummary) -> Result<()> {
    for (days, severity, fingerprint) in [
        (30, Severity::Warning, "domain_approval_expiry:30d"),
        (7, Severity::Error, "domain_approval_expiry:7d"),
    ] {
        let (count, earliest, domains): (i64, Option<DateTime<Utc>>, Option<Vec<String>>) =
            sqlx::query_as(
                "SELECT count(*), min(approval_expires_at),
                        (array_agg(domain ORDER BY approval_expires_at, domain))[1:20]
                 FROM benefit_source_domains
                 WHERE is_active AND crawl_allowed AND compliance_status = 'approved'
                   AND approval_expires_at > now()
                   AND approval_expires_at <= now() + make_interval(days => $1)",
            )
            .bind(days)
            .fetch_one(pool)
            .await?;
        if days == 30 {
            summary.expiring_30d = count;
        } else {
            summary.expiring_7d = count;
        }
        if count > 0 {
            let not_renewed: Value = summary
                .not_renewed
                .iter()
                .map(|(domain, reason)| json!({"domain": domain, "reason": reason}))
                .collect();
            let message = format!(
                "도메인 수집 승인 {count}건이 {days}일 안에 만료됩니다. 만료되면 신청 자료 확인이 멈춥니다."
            );
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint,
                    severity,
                    component: "domain_approvals",
                    event_type: "domain_approval_expiring",
                    subject_type: None,
                    subject_id: None,
                    message: &message,
                    details: json!({
                        "within_days": days,
                        "count": count,
                        "earliest_expires_at": earliest,
                        "domains": domains.unwrap_or_default(),
                        "not_renewed": not_renewed,
                    }),
                },
            )
            .await?;
        } else {
            operations::resolve_event(
                pool,
                fingerprint,
                None,
                Some("no approval expires inside the window"),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_approvals_independent_of_terms_and_agreements_are_renewable() {
        assert_eq!(
            renewal_block(Some("public_facts_review"), Some("not_found")),
            None
        );
        for (basis, outcome, reason) in [
            (
                "public_facts_review",
                Some("no_automated_access_restriction"),
                "terms_dependent_requires_review",
            ),
            (
                "terms_explicit",
                Some("explicit_permission"),
                "terms_dependent_requires_review",
            ),
            (
                "open_license",
                Some("not_found"),
                "terms_dependent_requires_review",
            ),
            (
                "written_permission",
                Some("explicit_permission"),
                "external_agreement_requires_review",
            ),
            (
                "partner_contract",
                Some("explicit_permission"),
                "external_agreement_requires_review",
            ),
            ("public_facts_review", None, "no_terms_classification"),
        ] {
            assert_eq!(renewal_block(Some(basis), outcome), Some(reason), "{basis}");
        }
        assert!(robots_unchanged(Some("ABC"), "abc"));
        assert!(!robots_unchanged(Some("abc"), "abd"));
        assert!(!robots_unchanged(None, "abc"));
    }

    #[test]
    fn renewal_is_off_unless_enabled() {
        if std::env::var("DOMAIN_APPROVAL_AUTO_RENEW").is_err() {
            assert_eq!(renewal_window_from_env().unwrap(), None);
        }
    }

    async fn insert(
        pool: &PgPool,
        domain: &str,
        basis: &str,
        outcome: &str,
        expires_in: &str,
    ) -> Uuid {
        sqlx::query_scalar(&format!(
            "INSERT INTO benefit_source_domains(organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
               compliance_status,approval_basis,approval_reference,terms_url,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
               allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,min_request_interval_ms)
             VALUES('renewal fixture',$1,'brand_official',true,'verified','fixture','approved',$2,'fixture',
               CASE WHEN $3='not_found' THEN NULL ELSE 'https://{domain}/terms' END,now(),$3,'allowed',now(),repeat('a',32),
               ARRAY['/apply'],ARRAY['/apply'],'https://mazimi.example/bot','fixture',now()+interval '{expires_in}'-interval '90 days',now()+interval '{expires_in}','direct_public',30000)
             RETURNING id"
        ))
        .bind(domain)
        .bind(basis)
        .bind(outcome)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn approvals_renew_only_on_identical_robots_and_the_rest_are_announced() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required");
        // Fixtures of an aborted earlier run would sit inside the windows.
        sqlx::query(
            "DELETE FROM benefit_source_domains
             WHERE domain ~ '^(renew|agreement|blocked|changed|terms)-[0-9a-f]{12}\\.example\\.org$'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let tag = Uuid::new_v4().simple().to_string()[..12].to_string();
        let name = |kind: &str| format!("{kind}-{tag}.example.org");
        let (renewable, agreement, blocked, changed, terms) = (
            name("renew"),
            name("agreement"),
            name("blocked"),
            name("changed"),
            name("terms"),
        );
        let ids = [
            insert(
                &pool,
                &renewable,
                "public_facts_review",
                "not_found",
                "10 days",
            )
            .await,
            insert(
                &pool,
                &agreement,
                "written_permission",
                "explicit_permission",
                "5 days",
            )
            .await,
            insert(
                &pool,
                &blocked,
                "public_facts_review",
                "not_found",
                "15 days",
            )
            .await,
            insert(
                &pool,
                &changed,
                "public_facts_review",
                "not_found",
                "25 days",
            )
            .await,
            insert(
                &pool,
                &terms,
                "terms_explicit",
                "explicit_permission",
                "20 days",
            )
            .await,
        ];
        let state = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (DateTime<Utc>, i32, Option<i32>)>(
                    "SELECT approval_expires_at, approval_auto_renewals, approval_period_days
                     FROM benefit_source_domains WHERE id=$1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let mut before = Vec::new();
        for id in ids {
            before.push(state(id).await.0);
        }
        let (blocked_domain, changed_domain) = (blocked.clone(), changed.clone());
        let summary = run(&pool, Some(30), |domain: String, _paths, _contact| {
            let allowed = domain != blocked_domain;
            // A robots.txt that still allows but changed since approval.
            let hash = if domain == changed_domain { "b" } else { "a" }.repeat(32);
            async move {
                Ok(RobotsPreflightResult {
                    robots_url: format!("https://{domain}/robots.txt"),
                    allowed,
                    checked_at: Utc::now(),
                    content_hash: hash,
                })
            }
        })
        .await
        .unwrap();

        let (renewed_at, renewals, period) = state(ids[0]).await;
        assert_eq!((renewals, period), (1, Some(90)));
        assert_eq!((renewed_at - before[0]).num_days(), 90);
        for (index, id) in ids.iter().enumerate().skip(1) {
            assert_eq!(state(*id).await, (before[index], 0, None), "{index}");
        }
        for (domain, reason) in [
            (&agreement, "external_agreement_requires_review"),
            (&blocked, "robots_disallows"),
            (&changed, "robots_changed_requires_review"),
            (&terms, "terms_dependent_requires_review"),
        ] {
            assert!(
                summary.not_renewed.contains(&(domain.clone(), reason)),
                "{domain} {reason}: {:?}",
                summary.not_renewed
            );
        }

        let evidence: Value = sqlx::query_scalar(
            "SELECT details FROM operational_events
             WHERE event_type='domain_approval_auto_renewed' AND subject_id=$1",
        )
        .bind(ids[0].to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(evidence["automatic"], true);
        assert_eq!(evidence["robots_unchanged"], true);
        assert_eq!(evidence["robots_hash"], "a".repeat(32));

        let notice = |fingerprint: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, String, Value)>(
                    "SELECT status, severity, details FROM operational_events WHERE fingerprint=$1",
                )
                .bind(fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (status, severity, details) = notice("domain_approval_expiry:30d").await;
        assert_eq!((status.as_str(), severity.as_str()), ("open", "warning"));
        let listed: Vec<String> = serde_json::from_value(details["domains"].clone()).unwrap();
        for domain in [&agreement, &blocked, &changed, &terms] {
            assert!(listed.contains(domain), "{domain}");
        }
        assert!(!listed.contains(&renewable), "renewed approvals drop out");
        let (status, severity, details) = notice("domain_approval_expiry:7d").await;
        assert_eq!((status.as_str(), severity.as_str()), ("open", "error"));
        let listed: Vec<String> = serde_json::from_value(details["domains"].clone()).unwrap();
        assert!(listed.contains(&agreement) && !listed.contains(&blocked));

        sqlx::query("DELETE FROM benefit_source_domains WHERE id = ANY($1)")
            .bind(ids.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        // With nothing left inside the windows the notices resolve.
        run(&pool, None, |_: String, _: Vec<String>, _: String| async {
            anyhow::bail!("not called")
        })
        .await
        .unwrap();
        assert_eq!(notice("domain_approval_expiry:7d").await.0, "resolved");
        pool.close().await;
    }
}
