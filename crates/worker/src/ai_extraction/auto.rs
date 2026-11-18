//! Auto-apply of rule-verified facts. A candidate is applied without a human
//! only when all of these hold:
//!
//! 1. its evidence is an exact span of the stored source (validator),
//! 2. confidence is HIGH and no validator warning (`pending` state),
//! 3. the deterministic cross-check parser reads the same value from the
//!    same quote ([`super::crosscheck`]),
//! 4. the merge guards pass and the plan overwrites nothing that already
//!    exists (an authoritative structured value or an earlier approval),
//! 5. no human rejected or reverted the same fact for the same source,
//! 6. its type is enabled, the global switch is on and the run cap is not
//!    reached.
//!
//! Applying goes through [`super::apply::apply`] with [`Actor::Auto`], one
//! transaction per fact. Everything else stays `pending` with the reason in
//! `auto_check` (`verdict`/`reason`, or `hold` for an agreeing fact that was
//! not applied).
use super::apply::{self, Actor, ApplyError, AutoProvenance, Decision, DecisionKind};
use super::crosscheck::PARSER_VERSION;
use super::facts::FactType;
use anyhow::{bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub const DEFAULT_TYPES: &str = "age,application_window,document";
pub const DEFAULT_MAX_PER_RUN: usize = 25;
pub const DEFAULT_AUDIT_PERCENT: u8 = 10;
pub const DEFAULT_AUDIT_SEED: &str = "mazimi-auto-audit-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoApplyConfig {
    /// Global kill switch (`EXTRACTION_AUTO_APPLY_ENABLED`, default off).
    pub enabled: bool,
    /// Fact types that may be applied automatically.
    pub types: Vec<FactType>,
    /// Facts applied per run at most; the rest wait for the next run or a
    /// human, and reaching the cap raises an operational event.
    pub max_per_run: usize,
    /// Percentage of auto-applied facts flagged for post-hoc human review.
    pub audit_percent: u8,
    pub audit_seed: String,
}

impl Default for AutoApplyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            types: parse_types(DEFAULT_TYPES).expect("default types parse"),
            max_per_run: DEFAULT_MAX_PER_RUN,
            audit_percent: DEFAULT_AUDIT_PERCENT,
            audit_seed: DEFAULT_AUDIT_SEED.into(),
        }
    }
}

pub fn parse_types(value: &str) -> Result<Vec<FactType>> {
    let mut types = Vec::new();
    for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let Some(fact_type) = FactType::parse(name) else {
            bail!("EXTRACTION_AUTO_APPLY_TYPES: unknown fact type '{name}'");
        };
        if !types.contains(&fact_type) {
            types.push(fact_type);
        }
    }
    Ok(types)
}

impl AutoApplyConfig {
    /// Reads `EXTRACTION_AUTO_APPLY_*` and `EXTRACTION_AUTO_AUDIT_*`.
    pub fn from_env() -> Result<Self> {
        let flag = |name: &str| {
            matches!(
                std::env::var(name)
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "true" | "1" | "yes" | "on"
            )
        };
        let number = |name: &str, default: u64, min: u64, max: u64| -> Result<u64> {
            let value = match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("{name} must be an integer"))?,
                _ => default,
            };
            if !(min..=max).contains(&value) {
                bail!("{name} must be between {min} and {max}");
            }
            Ok(value)
        };
        let types = match std::env::var("EXTRACTION_AUTO_APPLY_TYPES") {
            Ok(value) => parse_types(&value)?,
            Err(_) => parse_types(DEFAULT_TYPES)?,
        };
        let seed = std::env::var("EXTRACTION_AUTO_AUDIT_SEED")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_AUDIT_SEED.into());
        Ok(Self {
            enabled: flag("EXTRACTION_AUTO_APPLY_ENABLED"),
            types,
            max_per_run: number(
                "EXTRACTION_AUTO_APPLY_MAX_PER_RUN",
                DEFAULT_MAX_PER_RUN as u64,
                1,
                500,
            )? as usize,
            audit_percent: number(
                "EXTRACTION_AUTO_AUDIT_PERCENT",
                u64::from(DEFAULT_AUDIT_PERCENT),
                0,
                100,
            )? as u8,
            audit_seed: seed,
        })
    }
}

/// Deterministic audit sampling: `sha256(seed ‖ 0x00 ‖ candidate id)` read
/// as a number, modulo 100, below `percent`. The same seed and candidate
/// always give the same answer, so a sample can be reproduced and audited.
pub fn audit_sample(seed: &str, candidate_id: Uuid, percent: u8) -> bool {
    if percent == 0 {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update([0u8]);
    hasher.update(candidate_id.as_bytes());
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head) % 100 < u64::from(percent)
}

/// Per-run auto-apply counters (persisted on the run row).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AutoCounters {
    /// Pending facts the rule parser agreed with.
    pub verified: usize,
    pub applied: usize,
    /// Agreeing facts left for a human (switch off, type off, cap,
    /// conflict).
    pub held: usize,
    pub cap_skips: usize,
    pub audit_sampled: usize,
}

#[derive(Debug, FromRow)]
struct AgreeingRow {
    id: Uuid,
    fact_type: String,
    rule: Option<String>,
}

async fn hold(pool: &PgPool, candidate_id: Uuid, reason: &str) -> Result<()> {
    sqlx::query(
        "UPDATE program_ai_extraction_candidates
         SET auto_check = auto_check || jsonb_build_object('hold', $2::text)
         WHERE id = $1 AND status = 'pending'
           AND auto_check->>'hold' IS DISTINCT FROM $2",
    )
    .bind(candidate_id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Apply this program's rule-verified pending facts for the current source.
pub async fn auto_apply_program(
    pool: &PgPool,
    program_id: Uuid,
    source_hash: &str,
    run_id: Uuid,
    config: &AutoApplyConfig,
    counters: &mut AutoCounters,
) -> Result<()> {
    let rows = sqlx::query_as::<_, AgreeingRow>(
        "SELECT id, fact_type, auto_check->>'rule' AS rule
         FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND source_hash = $2 AND status = 'pending'
           AND auto_check->>'verdict' = 'agree' AND auto_check->>'parser_version' = $3
         ORDER BY fact_key, id",
    )
    .bind(program_id)
    .bind(source_hash)
    .bind(PARSER_VERSION)
    .fetch_all(pool)
    .await?;
    for row in rows {
        counters.verified += 1;
        let enabled_type =
            FactType::parse(&row.fact_type).is_some_and(|t| config.types.contains(&t));
        let reason = if !config.enabled {
            Some("auto_apply_disabled")
        } else if !enabled_type {
            Some("type_not_enabled")
        } else if counters.applied >= config.max_per_run {
            counters.cap_skips += 1;
            Some("run_cap_reached")
        } else {
            None
        };
        if let Some(reason) = reason {
            counters.held += 1;
            hold(pool, row.id, reason).await?;
            continue;
        }
        let provenance = AutoProvenance {
            run_id,
            rule: row.rule.clone().unwrap_or_default(),
            parser_version: PARSER_VERSION.into(),
            audit_sample: audit_sample(&config.audit_seed, row.id, config.audit_percent),
        };
        let mut tx = pool.begin().await?;
        let decision = Decision {
            candidate_id: row.id,
            decision: DecisionKind::Approve,
            confirm_overwrite: false,
        };
        match apply::apply(&mut tx, program_id, &[decision], &Actor::Auto(&provenance)).await {
            Ok(_) => {
                tx.commit().await?;
                counters.applied += 1;
                if provenance.audit_sample {
                    counters.audit_sampled += 1;
                }
                tracing::info!(%program_id, candidate_id = %row.id, rule = provenance.rule, audit = provenance.audit_sample, "rule-verified fact applied automatically");
            }
            Err(error) => {
                drop(tx);
                counters.held += 1;
                let reason = match &error {
                    ApplyError::Conflict { code, .. } => *code,
                    other => {
                        tracing::warn!(%program_id, candidate_id = %row.id, error = %crate::error_safety::persisted_error(other), "auto-apply failed; left for a reviewer");
                        "apply_error"
                    }
                };
                hold(pool, row.id, reason).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_scoped_to_the_three_low_risk_types() {
        let config = AutoApplyConfig::default();
        assert!(!config.enabled);
        assert_eq!(
            config.types,
            vec![
                FactType::Age,
                FactType::ApplicationWindow,
                FactType::Document
            ]
        );
        assert!(parse_types("age, region ,age").unwrap().len() == 2);
        assert!(parse_types("age,salary").is_err());
        assert!(parse_types("").unwrap().is_empty());
    }

    #[test]
    fn audit_sampling_is_deterministic_and_close_to_the_rate() {
        let ids: Vec<Uuid> = (0..2000u128).map(Uuid::from_u128).collect();
        let sample = |seed: &str, percent| -> Vec<bool> {
            ids.iter()
                .map(|id| audit_sample(seed, *id, percent))
                .collect()
        };
        assert_eq!(
            sample("seed-a", 10),
            sample("seed-a", 10),
            "same seed, same sample"
        );
        assert_ne!(
            sample("seed-a", 10),
            sample("seed-b", 10),
            "the seed changes the sample"
        );
        let hits = sample("seed-a", 10).iter().filter(|s| **s).count();
        assert!((140..=260).contains(&hits), "{hits} of 2000 at 10%");
        assert!(sample("seed-a", 0).iter().all(|s| !s));
        assert!(sample("seed-a", 100).iter().all(|s| *s));
    }
}
