//! Read-only health of the stored eligibility data: rows a data defect
//! hides or leaves unknown, which no request counts on its own.
//!
//! - Every active programme's latest `eligibility_rules` row through the
//!   rule engine's parser (hard filter and ranking rule). An unparseable hard
//!   filter hides the row in every list (`RuleUnavailable`); an unparseable
//!   ranking rule drops it to inline scoring and blocks `verified`.
//! - Programmes with a `region_districts` entry that is not a current code
//!   (`structural::programme_districts`): their district check is unknown.
//! - Inverted age bands (`min_age > max_age`): their age check is unknown.
//! - Coded income bands whose text states no ceiling the coded run contains
//!   (`income::decision_bound`): their income check is unknown.
//!
//! - Condition reviews stored `complete` that the coverage check has not
//!   passed at its current version (`coverage::served_status`): they serve
//!   as incomplete until re-imported (review M4).
//!
//! [`measure`] only reads. [`monitor`] (data maintenance, every run) also
//! opens the operational event [`UNPARSEABLE_FINGERPRINT`] while any rule
//! fails to parse, [`UNRESOLVED_DISTRICT_FINGERPRINT`] (naming the codes)
//! while a district entry is not a current code, and
//! [`REVIEWS_AWAITING_REIMPORT_FINGERPRINT`] while a stored `complete`
//! review serves as incomplete; each resolves when its count is 0. The
//! other counts are logged with them. The `rule-health` binary prints [`measure`] for the
//! pre-deploy check and exits non-zero on an unparseable rule.

use anyhow::Result;
use mazimi_core::models::Program;
use mazimi_core::{income, rule_engine, structural};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Event of active rules the parser rejects.
pub const UNPARSEABLE_FINGERPRINT: &str = "rule_health:unparseable";
/// Event of programmes listing a district code that is not current.
pub const UNRESOLVED_DISTRICT_FINGERPRINT: &str = "rule_health:unresolved_district";
/// Event of `complete` reviews that serve as incomplete until re-imported.
pub const REVIEWS_AWAITING_REIMPORT_FINGERPRINT: &str = "rule_health:reviews_awaiting_reimport";

/// How many programme ids each count keeps as a sample.
const SAMPLE: usize = 10;

/// One count and a sample of the programmes it counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Count {
    pub programmes: usize,
    pub sample: Vec<Uuid>,
}

impl Count {
    fn add(&mut self, id: Uuid) {
        self.programmes += 1;
        if self.sample.len() < SAMPLE {
            self.sample.push(id);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuleHealth {
    /// Active programmes with a stored rule version.
    pub rules_checked: usize,
    /// Hard filter fails to parse: the row is hidden.
    pub unparseable_hard_filter: Count,
    /// Ranking rule fails to parse: inline scoring, never verified.
    pub unparseable_rule: Count,
    pub unresolved_district: Count,
    /// The district entries behind [`Self::unresolved_district`], distinct,
    /// at most [`SAMPLE`].
    pub unresolved_district_codes: Vec<String>,
    /// `min_age > max_age`. Zero wherever the validated constraint
    /// `programs_age_quality_check` holds: the count guards a regression of
    /// that constraint (review2-verify gap 7), it measures nothing else.
    pub inverted_age_band: Count,
    pub unstated_income_bound: Count,
    /// Stored `complete` condition reviews of current programmes that serve
    /// as incomplete (no or an older `coverage_version`).
    pub reviews_awaiting_reimport: usize,
}

impl RuleHealth {
    pub fn unparseable(&self) -> usize {
        self.unparseable_hard_filter.programmes + self.unparseable_rule.programmes
    }
}

/// Whether a stored tree parses (an absent tree is fine).
fn parses(tree: Option<&Value>) -> bool {
    tree.is_none_or(|tree| tree.is_null() || rule_engine::parse_rule_json(tree).is_ok())
}

/// Counts over the rows given; the pure part of [`measure`].
pub fn tally(rules: &[(Uuid, Value, Option<Value>)], programmes: &[Program]) -> RuleHealth {
    let mut health = RuleHealth {
        rules_checked: rules.len(),
        ..RuleHealth::default()
    };
    for (id, rule_json, hard_filter_json) in rules {
        if !parses(hard_filter_json.as_ref()) {
            health.unparseable_hard_filter.add(*id);
        }
        if !parses(Some(rule_json)) {
            health.unparseable_rule.add(*id);
        }
    }
    for program in programmes {
        let unresolved: Vec<&String> = program
            .region_districts
            .iter()
            .flatten()
            .filter(|entry| {
                !matches!(
                    structural::current_district(entry),
                    structural::Current::Current(_)
                )
            })
            .collect();
        if !unresolved.is_empty() {
            health.unresolved_district.add(program.id);
            for entry in unresolved {
                if health.unresolved_district_codes.len() < SAMPLE
                    && !health.unresolved_district_codes.contains(entry)
                {
                    health.unresolved_district_codes.push(entry.clone());
                }
            }
        }
        if matches!((program.min_age, program.max_age), (Some(min), Some(max)) if min > max) {
            health.inverted_age_band.add(program.id);
        }
        if let Some(income::IncomeCondition::MedianBands(bands)) = income::condition(program) {
            if income::median_bound(&bands).is_some() && income::decision_bound(program).is_none() {
                health.unstated_income_bound.add(program.id);
            }
        }
    }
    health
}

/// Reads the counts. Programmes are the current catalogue
/// (`current_programs`); rules are each one's latest version.
pub async fn measure(pool: &PgPool) -> Result<RuleHealth> {
    let rules: Vec<(Uuid, Value, Option<Value>)> = sqlx::query_as(
        "SELECT DISTINCT ON (r.program_id) r.program_id, r.rule_json, r.hard_filter_json
         FROM eligibility_rules r JOIN current_programs p ON p.id = r.program_id
         ORDER BY r.program_id, r.version DESC",
    )
    .fetch_all(pool)
    .await?;
    // Only rows a count can hit, with the text the income reader needs.
    let programmes: Vec<Program> = sqlx::query_as(
        "SELECT * FROM current_programs
         WHERE region_districts IS NOT NULL
            OR (min_age IS NOT NULL AND max_age IS NOT NULL AND min_age > max_age)
            OR jsonb_typeof(normalized_payload->'income_median_bands') = 'array'",
    )
    .fetch_all(pool)
    .await?;
    let mut health = tally(&rules, &programmes);
    let reviews: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_condition_reviews r
         JOIN current_programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         WHERE r.status = 'complete'
           AND (r.coverage_version IS NULL OR r.coverage_version < $1)",
    )
    .bind(mazimi_core::condition_review::coverage::VERSION)
    .fetch_one(pool)
    .await?;
    health.reviews_awaiting_reimport = usize::try_from(reviews).unwrap_or(0);
    Ok(health)
}

/// [`measure`], logged, with the unparseable-rule event opened or resolved.
pub async fn monitor(pool: &PgPool) -> Result<RuleHealth> {
    use crate::operations::{self, EventCommand, Severity};
    let health = measure(pool).await?;
    tracing::info!(
        rules_checked = health.rules_checked,
        unparseable_hard_filter = health.unparseable_hard_filter.programmes,
        unparseable_rule = health.unparseable_rule.programmes,
        unresolved_district = health.unresolved_district.programmes,
        inverted_age_band = health.inverted_age_band.programmes,
        unstated_income_bound = health.unstated_income_bound.programmes,
        reviews_awaiting_reimport = health.reviews_awaiting_reimport,
        "rule health measured"
    );
    let details = serde_json::to_value(&health)?;
    raise_while(
        pool,
        health.unresolved_district.programmes > 0,
        &EventCommand {
            fingerprint: UNRESOLVED_DISTRICT_FINGERPRINT,
            severity: Severity::Warning,
            component: "rule_health",
            event_type: "district_unresolved",
            subject_type: None,
            subject_id: None,
            message: "현재 시·군·구 표에 없는 지역 코드를 가진 공고가 있습니다. 해당 시·도 사용자에게는 시·군·구 확인 필요로 보입니다. details의 unresolved_district_codes를 표에 반영하세요.",
            details: details.clone(),
        },
        "every listed district is current",
    )
    .await?;
    raise_while(
        pool,
        health.reviews_awaiting_reimport > 0,
        &EventCommand {
            fingerprint: REVIEWS_AWAITING_REIMPORT_FINGERPRINT,
            severity: Severity::Warning,
            component: "rule_health",
            event_type: "reviews_awaiting_reimport",
            subject_type: None,
            subject_id: None,
            message: "완료로 저장된 조건 검토 중 현재 검증을 거치지 않은 것이 있어 미완료로 제공됩니다. 검토 파일을 다시 가져오세요.",
            details,
        },
        "every complete review passed the current coverage check",
    )
    .await?;
    if health.unparseable() > 0 {
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: UNPARSEABLE_FINGERPRINT,
                severity: if health.unparseable_hard_filter.programmes > 0 {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                component: "rule_health",
                event_type: "rule_unparseable",
                subject_type: None,
                subject_id: None,
                message: "저장된 자격 규칙 중 규칙 엔진이 읽지 못하는 것이 있습니다. 필수 조건이 읽히지 않는 공고는 목록에서 숨겨집니다. 규칙을 확인하세요.",
                details: serde_json::to_value(&health)?,
            },
        )
        .await?;
    } else {
        operations::resolve_event(
            pool,
            UNPARSEABLE_FINGERPRINT,
            None,
            Some("every active rule parses"),
        )
        .await?;
    }
    Ok(health)
}

/// Opens `event` while `open`, else resolves its fingerprint.
async fn raise_while(
    pool: &PgPool,
    open: bool,
    event: &crate::operations::EventCommand<'_>,
    resolved_note: &str,
) -> Result<()> {
    if open {
        crate::operations::record_event(pool, event).await?;
    } else {
        crate::operations::resolve_event(pool, event.fingerprint, None, Some(resolved_note))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn program(extra: Value) -> Program {
        let mut base = json!({
            "id": Uuid::new_v4(), "program_type": "policy", "source_type": "gov_benefits",
            "title": "t", "program_status": "active", "is_active": true,
            "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now()
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    /// Each defect is counted once, on the programme that has it.
    #[test]
    fn every_defect_kind_is_counted() {
        let typed = json!({"field": "age", "op": "gte", "value": 19});
        let mistyped = json!({"field": "age", "op": "gte", "value": "19"});
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let rules = vec![
            (ids[0], typed.clone(), Some(typed.clone())),
            (ids[1], typed.clone(), Some(mistyped.clone())),
            (ids[2], mistyped, None),
        ];
        let districts = program(json!({"region_districts": ["수영구", "26999", "26410"]}));
        let inverted = program(json!({"min_age": 35, "max_age": 19}));
        let unstated = program(
            json!({"normalized_payload": {"income_median_bands": ["0-50"]},
            "raw_payload": {"지원대상": "관내 청년 누구나"}}),
        );
        let stated = program(
            json!({"normalized_payload": {"income_median_bands": ["0-50"]},
            "raw_payload": {"지원대상": "기준 중위소득 50% 이하 청년"}}),
        );
        let clean = program(json!({"region_districts": ["26410"], "min_age": 19, "max_age": 34}));
        let health = tally(
            &rules,
            &[
                districts.clone(),
                inverted.clone(),
                unstated.clone(),
                stated,
                clean,
            ],
        );
        assert_eq!(health.rules_checked, 3);
        assert_eq!(health.unparseable_hard_filter.sample, [ids[1]]);
        assert_eq!(health.unparseable_rule.sample, [ids[2]]);
        assert_eq!(health.unparseable(), 2);
        assert_eq!(health.unresolved_district.sample, [districts.id]);
        assert_eq!(health.unresolved_district_codes, ["수영구", "26999"]);
        assert_eq!(health.inverted_age_band.sample, [inverted.id]);
        assert_eq!(health.unstated_income_bound.sample, [unstated.id]);
    }

    /// Review2-verify gap 6: `measure` reads the stored rules and `monitor`
    /// opens the unparseable-rule event as an error while a hard filter
    /// fails to parse, and resolves it once none does.
    #[tokio::test]
    async fn monitor_opens_and_resolves_the_unparseable_event() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = PgPool::connect(&url).await.unwrap();
        let before = measure(&pool).await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active) VALUES($1,'policy','manual',$1::text,'규칙 점검','active',true)")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mistyped = json!({"field": "age", "op": "gte", "value": "19"});
        sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,compiled_at) VALUES($1,$2,$2,1,now())")
            .bind(id)
            .bind(&mistyped)
            .execute(&pool)
            .await
            .unwrap();
        let health = monitor(&pool).await.unwrap();
        assert_eq!(
            health.unparseable_hard_filter.programmes,
            before.unparseable_hard_filter.programmes + 1
        );
        assert_eq!(health.rules_checked, before.rules_checked + 1);
        let event = |pool: PgPool| async move {
            sqlx::query_as::<_, (String, String)>(
                "SELECT status, severity FROM operational_events WHERE fingerprint = $1",
            )
            .bind(UNPARSEABLE_FINGERPRINT)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(event(pool.clone()).await, ("open".into(), "error".into()));
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let health = monitor(&pool).await.unwrap();
        assert_eq!(health.unparseable(), before.unparseable());
        if before.unparseable() == 0 {
            assert_eq!(event(pool.clone()).await.0, "resolved");
        }
    }

    /// Review B6 and M4: an unresolved district code and a `complete`
    /// review that serves as incomplete each open their event, naming the
    /// code; both resolve once nothing is left.
    #[tokio::test]
    async fn monitor_raises_unresolved_districts_and_reviews_awaiting_reimport() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = PgPool::connect(&url).await.unwrap();
        let before = measure(&pool).await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,regions,region_districts) VALUES($1,'policy','manual',$1::text,'지역 점검','active',true,ARRAY['busan'],ARRAY['26999'])")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO program_condition_reviews(source_type,source_id,status,window_json,text_hash,reviewer) VALUES('manual',$1::text,'complete',jsonb_build_object('kind','unknown'),repeat('a',64),'pilot')")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let health = monitor(&pool).await.unwrap();
        assert_eq!(
            health.unresolved_district.programmes,
            before.unresolved_district.programmes + 1
        );
        assert!(health
            .unresolved_district_codes
            .contains(&"26999".to_owned()));
        assert_eq!(
            health.reviews_awaiting_reimport,
            before.reviews_awaiting_reimport + 1
        );
        let status = |fingerprint: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT status FROM operational_events WHERE fingerprint = $1",
                )
                .bind(fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(status(UNRESOLVED_DISTRICT_FINGERPRINT).await, "open");
        assert_eq!(status(REVIEWS_AWAITING_REIMPORT_FINGERPRINT).await, "open");
        sqlx::query("DELETE FROM program_condition_reviews WHERE source_id = $1::text")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        monitor(&pool).await.unwrap();
        if before.unresolved_district.programmes == 0 {
            assert_eq!(status(UNRESOLVED_DISTRICT_FINGERPRINT).await, "resolved");
        }
        if before.reviews_awaiting_reimport == 0 {
            assert_eq!(
                status(REVIEWS_AWAITING_REIMPORT_FINGERPRINT).await,
                "resolved"
            );
        }
    }
}
