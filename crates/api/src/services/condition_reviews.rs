//! Serve-time reading of `program_condition_reviews` together with the
//! applicant review the predicate needs. See
//! [`mazimi_core::condition_review`].

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use mazimi_core::applicant_review::{self, ApplicantClass};
use mazimi_core::condition_review::{self, overrides::CodeOverride, ServeReview};
use serde_json::Value;
use uuid::Uuid;

/// What the list knows about one programme's reviews.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReviewFacts {
    /// The condition review, current or stale (`ServeReview::current`).
    pub review: Option<ServeReview>,
    /// The applicant class, only while its review is current.
    pub applicant: Option<ApplicantClass>,
}

type Row = (
    Uuid,
    String,
    Option<Value>,
    Option<String>,
    Option<Value>,
    Option<Value>,
    Option<Vec<String>>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Value>,
    Option<i16>,
);

/// Reviews of the given programmes, keyed by programme id. Only the hashed
/// fields of `raw_payload` are read, and only for reviewed rows.
pub(crate) async fn load(
    connection: &mut sqlx::PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, ReviewFacts>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT p.id, p.source_type, {hashed} AS hashed,
                r.status, r.rule_json, r.window_json, r.remaining_checks,
                r.window_confirmed_at, r.text_hash, a.class, a.text_hash, r.code_overrides,
                r.coverage_version
         FROM current_programs p
         LEFT JOIN program_condition_reviews r
           ON r.source_type = p.source_type AND r.source_id = p.source_id
         LEFT JOIN program_applicant_reviews a
           ON a.source_type = p.source_type AND a.source_id = p.source_id
         WHERE p.id = ANY($1) AND (r.source_id IS NOT NULL OR a.source_id IS NOT NULL)",
        hashed = condition_review::hashed_projection_sql("p."),
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                source_type,
                hashed,
                status,
                rule_json,
                window_json,
                remaining,
                confirmed_at,
                stored_hash,
                class,
                applicant_hash,
                code_overrides,
                coverage_version,
            )| {
                let raw = hashed.unwrap_or(Value::Null);
                let review = match (status, window_json, stored_hash) {
                    (Some(status), Some(window_json), Some(stored)) => {
                        let current = condition_review::text_hash(&source_type, &raw);
                        Some(
                            ServeReview::from_columns(
                                // A `complete` row the coverage check did
                                // not write is served incomplete (audit H1).
                                condition_review::coverage::served_status(
                                    &status,
                                    coverage_version,
                                ),
                                rule_json.as_ref(),
                                &window_json,
                                remaining.unwrap_or_default(),
                                confirmed_at,
                                current.as_deref() == Some(stored.as_str()),
                            )
                            .with_code_overrides(code_overrides.as_ref()),
                        )
                    }
                    _ => None,
                };
                let applicant = match (class, applicant_hash) {
                    (Some(class), Some(stored))
                        if applicant_review::text_hash(&source_type, &raw).as_deref()
                            == Some(stored.as_str()) =>
                    {
                        ApplicantClass::parse(&class)
                    }
                    _ => None,
                };
                (id, ReviewFacts { review, applicant })
            },
        )
        .collect())
}

/// The code overrides of every listed programme whose review is current,
/// keyed by programme id. The list reads them before its structural pass,
/// so an overridden age, region, district or enrollment rule does not hide
/// the row there. Only reviews with overrides are read (partial index
/// `program_condition_reviews_code_overrides`), each joined to its
/// programme by the unique source key.
/// `ids` limits the read to those programmes (the assistant's cards); `None`
/// reads every one (the list).
pub(crate) async fn load_overrides(
    connection: &mut sqlx::PgConnection,
    ids: Option<&[Uuid]>,
) -> Result<HashMap<Uuid, Vec<CodeOverride>>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<Value>, String, Value)>(&format!(
        "SELECT p.id, p.source_type, {hashed} AS hashed, r.text_hash, r.code_overrides
         FROM program_condition_reviews r
         JOIN current_programs p
           ON p.source_type = r.source_type AND p.source_id = r.source_id
         WHERE r.code_overrides IS NOT NULL AND ($1::uuid[] IS NULL OR p.id = ANY($1))",
        hashed = condition_review::hashed_projection_sql("p."),
    ))
    .bind(ids.map(<[Uuid]>::to_vec))
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, source_type, hashed, stored, value)| {
            let raw = hashed.unwrap_or(Value::Null);
            let current = condition_review::text_hash(&source_type, &raw)?;
            (current == stored)
                .then(|| condition_review::overrides::parse_stored(Some(&value)).ok())
                .flatten()
                .map(|list| (id, list))
        })
        .collect())
}

/// The overrides of `program` that apply now (target present, replacement
/// still wider), read against its active hard filter.
pub(crate) fn applied<'a>(
    overrides: &'a HashMap<Uuid, Vec<CodeOverride>>,
    program: &mazimi_core::models::Program,
    hard_filter: Option<&Value>,
) -> Vec<&'a CodeOverride> {
    overrides.get(&program.id).map_or_else(Vec::new, |list| {
        condition_review::overrides::applicable(
            list,
            &condition_review::overrides::CodeColumns::of(program, hard_filter),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::recommendations::{evaluate_ranked, Relevance, ReviewContext};
    use chrono::TimeZone;
    use mazimi_core::condition_review::{pilot, store};
    use mazimi_core::models::{
        EligibilityRule, ExclusionReason, ProfileInput, Program, RecommendationItem,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    /// A coverage map labelling every segment of `raw` as no condition (the
    /// tests' rules come from code rules or overrides).
    fn covered(raw: &Value) -> Value {
        let labels: serde_json::Map<String, Value> =
            condition_review::coverage::segments("youth_center", raw)
                .unwrap()
                .into_iter()
                .map(|segment| (segment.seg_id, json!({"label": "non_condition"})))
                .collect();
        json!({ "labels": labels })
    }

    /// 12:00 KST on `date`.
    fn noon(date: &str) -> DateTime<Utc> {
        let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        Utc.from_utc_datetime(&d.and_hms_opt(3, 0, 0).unwrap())
    }

    fn program(id: Uuid, fields: &Value) -> Program {
        let now = Utc::now();
        let get = |key: &str| fields.get(key).cloned().unwrap_or(Value::Null);
        let mut normalized = serde_json::Map::new();
        for key in [
            "enrollment_statuses",
            "youth_conditions",
            "income_median_bands",
        ] {
            if let Some(value) = fields.get(key) {
                normalized.insert(key.to_owned(), value.clone());
            }
        }
        let normalized = if normalized.is_empty() {
            Value::Null
        } else {
            Value::Object(normalized)
        };
        serde_json::from_value(json!({
            "id": id, "program_type": get("program_type"), "source_type": "youth_center",
            "source_id": get("source_id"), "title": get("title").as_str().unwrap_or("검증 사업"), "summary": null,
            "provider_name": null, "official_url": null, "program_status": "active",
            "application_start_at": null, "application_end_at": null,
            "benefit_amount_monthly": null, "benefit_amount_semester": null,
            "benefit_amount_once": null, "region_scope": null, "school_scope": null, "tags": null,
            "raw_payload": null, "normalized_payload": normalized,
            "min_age": get("min_age"), "max_age": get("max_age"), "regions": get("regions"),
            "region_districts": get("region_districts"), "deadline_at": null, "is_active": true,
            "last_synced_at": null, "created_at": now, "updated_at": now, "company_name": null,
            "company_logo_url": null, "benefit_category": null, "provider_kind": null,
            "application_steps": null, "application_method": null,
            "submission_documents": null, "screening_method": null
        }))
        .unwrap()
    }

    /// Review facts as the list would load them right after the importers
    /// accepted `conditions` and `applicants` for these payloads, each
    /// validated against its programme's columns as `store::upsert` does.
    fn imported_facts(
        conditions: &[Value],
        applicants: &[Value],
        payloads: &HashMap<String, (Uuid, Value)>,
        programmes: &HashMap<Uuid, Program>,
    ) -> (HashMap<Uuid, ReviewFacts>, Vec<(String, String)>) {
        let mut facts: HashMap<Uuid, ReviewFacts> = HashMap::new();
        let mut rejected = Vec::new();
        for line in conditions {
            let input: store::ReviewInput = serde_json::from_value(line.clone()).unwrap();
            let Some((id, raw)) = payloads.get(&input.source_id) else {
                rejected.push((input.source_id.clone(), "program_not_found".into()));
                continue;
            };
            let columns = programmes
                .get(id)
                .map(|p| condition_review::overrides::CodeColumns::of(p, None))
                .unwrap_or_default();
            match store::validate_with(&input, raw, true, &columns) {
                Ok(valid) => {
                    let current = condition_review::text_hash("youth_center", raw)
                        == Some(valid.text_hash.clone());
                    facts.entry(*id).or_default().review = Some(ServeReview::from_columns(
                        valid.status.as_str(),
                        valid.rule_json.as_ref(),
                        &valid.window_json,
                        valid.remaining_checks.clone(),
                        None,
                        current,
                    ));
                }
                Err(reason) => rejected.push((input.source_id.clone(), format!("{reason:?}"))),
            }
        }
        for line in applicants {
            let source_id = line["source_id"].as_str().unwrap();
            let (id, raw) = &payloads[source_id];
            // The applicant importer's checks (applicant_review::upsert).
            let current = applicant_review::text_hash("youth_center", raw);
            let evidence = line["evidence"].as_str().unwrap();
            if current.as_deref() != line["text_hash"].as_str()
                || !applicant_review::evidence_found("youth_center", raw, evidence)
            {
                rejected.push((source_id.to_owned(), "applicant".into()));
                continue;
            }
            facts.entry(*id).or_default().applicant =
                ApplicantClass::parse(line["class"].as_str().unwrap());
        }
        (facts, rejected)
    }

    /// Phase 0 pilot replay: the merged reviews (converted) against the
    /// pilot's programmes and personas reproduce its two verified rows, and
    /// every converted programme gets the pilot's verdict and hide.
    #[test]
    fn pilot_reviews_reproduce_the_pilot_verified_rows() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/verified_pilot/fixture.json"
        ))
        .unwrap();
        let reviews = include_str!("../../tests/fixtures/verified_pilot/reviews.jsonl");
        let now = noon(fixture["now"].as_str().unwrap());
        // Ages as on the pilot date, whatever year the test runs in.
        let year_shift = mazimi_core::seoul_year(Utc::now()) - 2026;

        let mut payloads = HashMap::new();
        let mut programmes = HashMap::new();
        let mut applicant_hashes = HashMap::new();
        for p in fixture["programmes"].as_array().unwrap() {
            let pid = p["program_id"].as_str().unwrap().to_owned();
            let id: Uuid = pid.parse().unwrap();
            let source_id = p["source_id"].as_str().unwrap().to_owned();
            payloads.insert(source_id, (id, p["raw_payload"].clone()));
            programmes.insert(id, program(id, p));
            if let Some(hash) = p["applicant_text_hash"].as_str() {
                applicant_hashes.insert(pid, hash.to_owned());
            }
        }
        // The pilot's condition hashes are reproduced from the same text.
        for line in reviews.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            if let Some((_, raw)) = record["source_id"].as_str().and_then(|s| payloads.get(s)) {
                assert_eq!(
                    condition_review::text_hash("youth_center", raw).as_deref(),
                    record["text_hash"].as_str(),
                    "{}",
                    record["program_id"]
                );
            }
        }

        let conversion = pilot::convert(reviews, "pilot:verified-p0", &applicant_hashes);
        let (facts, rejected) = imported_facts(
            &conversion.conditions,
            &conversion.applicants,
            &payloads,
            &programmes,
        );
        // Since the review vocabulary is the rule engine's field table (audit
        // M4), marital_status records convert; this one's code rule lists
        // pilot values ("single", "engaged") outside the field's domain, so
        // the importer rejects it and it stays unreviewed.
        let rejected: Vec<&str> = rejected.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            rejected,
            ["20260922005400113506"],
            "importer would reject: {rejected:?}"
        );
        let converted: BTreeSet<Uuid> = conversion
            .conditions
            .iter()
            .map(|line| payloads[line["source_id"].as_str().unwrap()].0)
            .collect();
        // 55 youth_center records less 6 with rule fields the engine lacks
        // (education_stage and major_category exist since feat/code-rules,
        // marital_status is a review field since audit M4).
        assert_eq!(conversion.conditions.len(), 49, "{:?}", conversion.skipped);

        let personas: HashMap<String, ProfileInput> = fixture["personas"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, body)| {
                let mut body = body.clone();
                body["birth_year"] =
                    json!(body["birth_year"].as_i64().unwrap() + year_shift as i64);
                (name.clone(), serde_json::from_value(body).unwrap())
            })
            .collect();
        let mut verified = BTreeSet::new();
        let mut differences = Vec::new();
        let mut hidden: BTreeMap<&str, usize> = BTreeMap::new();
        // Pilot hides on programmes the converter skipped (unsupported
        // rule fields): they stay listed until those fields exist.
        let mut pilot_only_hides = Vec::new();
        let mut code_leaf_hides = Vec::new();
        for row in fixture["rows"].as_array().unwrap() {
            let id: Uuid = row["program_id"].as_str().unwrap().parse().unwrap();
            let Some(prog) = programmes.get(&id) else {
                // gov_benefits / local_welfare: no condition reviews yet.
                assert_eq!(row["verified"], false);
                continue;
            };
            let persona = row["persona"].as_str().unwrap();
            let result = evaluate_ranked(
                prog,
                None,
                &personas[persona],
                None,
                None::<&EligibilityRule>,
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts: facts.get(&id),
                    now,
                    section_enabled: true,
                },
            );
            let (is_verified, hide) = match &result {
                Ok((_, item)) => (item.verified, None),
                Err(ExclusionReason::HardRule) => (false, Some("HardRule")),
                Err(ExclusionReason::NotOpen) => (false, Some("NotOpen")),
                Err(other) => panic!("{persona} {id}: structurally excluded {other:?}"),
            };
            if is_verified {
                verified.insert((persona.to_owned(), id.to_string()[..8].to_owned()));
            }
            if let Some(hide) = hide {
                *hidden.entry(hide).or_insert(0) += 1;
            }
            if !converted.contains(&id) && row["hide"].is_string() {
                pilot_only_hides.push((persona.to_owned(), id.to_string()[..8].to_owned()));
            }
            // The pilot hid on any failed rule; here a copied source-code
            // leaf only withholds `verified` (hiding on codes is the compiled
            // eligibility_rules hard filter's job). That is the one allowed
            // difference: pilot HardRule, listed here with a rule mismatch.
            let code_leaf_only = hide.is_none()
                && row["hide"] == "HardRule"
                && result.as_ref().is_ok_and(|(_, item)| {
                    item.remaining_checks
                        .iter()
                        .any(|c| c.kind == "rule_mismatch")
                });
            if code_leaf_only {
                // The failing leaf is a copied source-code leaf, never a
                // reviewed one.
                use mazimi_core::rule_engine::{self as engine, RuleVerdict};
                let review = facts[&id].review.as_ref().unwrap();
                let verdict = |node: &Option<engine::RuleNode>| {
                    node.as_ref().map(|node| {
                        engine::evaluate(&personas[persona], node, &engine::AgeBasis::today())
                            .verdict
                    })
                };
                assert_eq!(
                    verdict(&review.code_rule),
                    Some(RuleVerdict::Fail),
                    "{persona} {id}"
                );
                assert_ne!(
                    verdict(&review.rule),
                    Some(RuleVerdict::Fail),
                    "{persona} {id}"
                );
                code_leaf_hides.push((persona.to_owned(), id.to_string()[..8].to_owned()));
            } else if converted.contains(&id)
                && (is_verified != row["verified"].as_bool().unwrap()
                    || hide != row["hide"].as_str())
            {
                differences.push((persona.to_owned(), id, is_verified, hide, row.clone()));
            }
        }
        assert_eq!(
            verified,
            BTreeSet::from([
                ("P1".to_owned(), "0409b1db".to_owned()),
                ("P3".to_owned(), "40c3e1dc".to_owned()),
            ])
        );
        assert!(differences.is_empty(), "{differences:#?}");
        // The pilot hid these on a copied education_stage code leaf; here
        // they stay listed (확인 필요) and are never verified.
        let mut expected: Vec<(String, String)> = [
            ("P1", "a86a1b98"),
            ("P1", "e67df565"),
            ("P1", "3b04e2c4"),
            ("P2", "71dc724f"),
            ("P4", "71dc724f"),
            ("P5", "71dc724f"),
        ]
        .map(|(p, i)| (p.to_owned(), i.to_owned()))
        .to_vec();
        expected.sort();
        let mut found = code_leaf_hides.clone();
        found.sort();
        assert_eq!(found, expected);
        eprintln!("pilot replay: verified {verified:?}, hidden {hidden:?}, pilot-only hides {pilot_only_hides:?}, code-leaf hides listed {code_leaf_hides:?}");

        // The rolling confirmation as if the official record still said 상시
        // for every candidate (the job's best case). The two 상시-coded
        // single-day events of plan §2.3 must stay unverified.
        let mut confirmed = facts.clone();
        for (id, raw) in payloads.values() {
            let Some(review) = confirmed.get_mut(id).and_then(|f| f.review.as_mut()) else {
                continue;
            };
            if review.status == condition_review::ReviewStatus::Complete
                && matches!(
                    review.window.kind,
                    condition_review::WindowKind::Rolling | condition_review::WindowKind::Unknown
                )
                && raw["aplyPrdSeCd"] == condition_review::YOUTH_ROLLING_CODE
            {
                review.window.kind = condition_review::WindowKind::Rolling;
                review.window_confirmed_at = Some(now - chrono::Duration::days(1));
            }
        }
        let mut rolling_verified = BTreeSet::new();
        for row in fixture["rows"].as_array().unwrap() {
            let id: Uuid = row["program_id"].as_str().unwrap().parse().unwrap();
            let Some(prog) = programmes.get(&id) else {
                continue;
            };
            let persona = row["persona"].as_str().unwrap();
            if let Ok((_, item)) = evaluate_ranked(
                prog,
                None,
                &personas[persona],
                None,
                None::<&EligibilityRule>,
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts: confirmed.get(&id),
                    now,
                    section_enabled: true,
                },
            ) {
                if item.verified {
                    rolling_verified.insert((persona.to_owned(), id.to_string()[..8].to_owned()));
                }
            }
        }
        for event in ["59b89356", "d2656e3d"] {
            assert!(
                !rolling_verified.iter().any(|(_, id)| id == event),
                "{event}: {rolling_verified:?}"
            );
        }
        assert!(verified.is_subset(&rolling_verified));
        eprintln!(
            "pilot replay with every 상시 window confirmed: {} rows {rolling_verified:?}",
            rolling_verified.len()
        );
    }

    /// Merge review M2: with `VERIFIED_SECTION_ENABLED` off a verified row
    /// keeps today's status, checks and place; reviewed facts still hide.
    #[test]
    fn verified_display_and_order_follow_the_section_flag() {
        use crate::services::recommendations::rank_items;
        let now = noon("2026-10-15");
        let year = mazimi_core::seoul_year(Utc::now());
        let prog = program(
            Uuid::new_v4(),
            &json!({"program_type": "policy", "min_age": 19, "max_age": 39, "regions": ["busan"]}),
        );
        let facts = ReviewFacts {
            review: Some(ServeReview::from_columns(
                "complete",
                Some(
                    &json!({"all": [{"id": "R1", "field": "employment_status", "op": "in",
                    "value": ["unemployed"], "origin": "review", "evidence": "x"}]}),
                ),
                &window("open"),
                Vec::new(),
                None,
                true,
            )),
            applicant: Some(ApplicantClass::Individual),
        };
        let profile = |job: &str| -> ProfileInput {
            serde_json::from_value(json!({"birth_year": year - 25, "region_code": "busan",
                "employment_status": job}))
            .unwrap()
        };
        let run = |job: &str, facts: Option<&ReviewFacts>, on: bool| {
            evaluate_ranked(
                &prog,
                None,
                &profile(job),
                None,
                None,
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts,
                    now,
                    section_enabled: on,
                },
            )
        };
        let baseline = run("unemployed", None, false).unwrap().1;
        assert_eq!(baseline.eligibility_status, "unknown");

        let on = run("unemployed", Some(&facts), true).unwrap().1;
        assert!(on.verified);
        assert_eq!(on.eligibility_status, "pass");
        assert!(on.conditions_verified && on.missing_checks.is_empty());

        let off = run("unemployed", Some(&facts), false).unwrap().1;
        // Review 2026-10-21 M2: `verified` is the only eligibility claim a
        // client may rest on, so it is served false while the section is off.
        assert!(!off.verified, "no claim while the section is off");
        assert!(off.remaining_checks.is_empty(), "nothing left to check");
        assert_eq!(off.eligibility_status, baseline.eligibility_status);
        assert_eq!(off.conditions_verified, baseline.conditions_verified);
        assert_eq!(off.missing_checks, baseline.missing_checks);

        // Reviewed facts hide in both states.
        for flag in [false, true] {
            assert_eq!(
                run("employed", Some(&facts), flag).err(),
                Some(ExclusionReason::HardRule)
            );
        }

        // Order: verified first only with the flag on. `verified` is served
        // only with the flag on, so the row ranked is the one the server
        // would carry then.
        let mut checked = baseline.clone();
        checked.eligibility_status = "pass".into();
        checked.program_id = Uuid::new_v4();
        let mut off = off.clone();
        off.verified = true;
        let rows = vec![(10.0, off.clone()), (90.0, checked.clone())];
        let ids = |rows: &[(f64, RecommendationItem)]| -> Vec<Uuid> {
            rows.iter().map(|(_, i)| i.program_id).collect()
        };
        let mut flag_off = rows.clone();
        rank_items(&mut flag_off, &Default::default(), false);
        assert_eq!(ids(&flag_off), [checked.program_id, off.program_id]);
        let mut flag_on = rows;
        rank_items(&mut flag_on, &Default::default(), true);
        assert_eq!(ids(&flag_on), [off.program_id, checked.program_id]);
    }

    // ── Property: verified ⇒ pass ───────────────────────────────────────

    fn window(kind: &str) -> Value {
        match kind {
            "open" => json!({"kind": "dated", "opens_at": "2026-10-01", "closes_at": "2026-10-31"}),
            "past" => json!({"kind": "dated", "opens_at": "2026-09-01", "closes_at": "2026-09-30"}),
            "rolling" => json!({"kind": "rolling"}),
            other => json!({"kind": other}),
        }
    }

    #[test]
    fn verified_implies_pass_and_nothing_missing() {
        let now = noon("2026-10-15");
        let year = mazimi_core::seoul_year(Utc::now());
        let profiles: Vec<ProfileInput> = [
            json!({"birth_year": year - 25, "region_code": "busan", "employment_status": "unemployed"}),
            json!({"birth_year": year - 25, "region_code": "busan", "city_code": "26410",
                   "employment_status": "employed", "income_bracket": 3}),
            json!({"birth_year": year - 35, "region_code": "busan"}),
            json!({"birth_year": year - 25, "region_code": "busan", "employment_status": "student",
                   "enrollment_status": "enrolled"}),
        ]
        .map(|p| serde_json::from_value(p).unwrap())
        .to_vec();
        let shapes = [
            json!({"program_type": "policy", "min_age": 19, "max_age": 34, "regions": ["busan"]}),
            json!({"program_type": "policy", "regions": ["busan"], "region_districts": ["26410"]}),
            json!({"program_type": "corporate_benefit"}),
            json!({"program_type": "policy", "min_age": 19, "max_age": 39,
                   "enrollment_statuses": ["enrolled"]}),
            // Audit M2: an income condition the list leaves unknown.
            json!({"program_type": "policy", "regions": ["busan"],
                   "youth_conditions": {"income_condition_text": "중위소득 150% 이하"}}),
        ];
        let rule_rows: Vec<Option<EligibilityRule>> = [
            None,
            Some(json!({"field": "employment_status", "op": "in",
                        "value": ["unemployed", "preparing", "student"], "origin": "source_codes"})),
            Some(json!({"field": "income_bracket", "op": "lte", "value": 5})),
        ]
        .into_iter()
        .map(|node| {
            node.map(|node| {
                serde_json::from_value(json!({
                    "id": Uuid::new_v4(), "program_id": Uuid::nil(),
                    "rule_json": {"all": [node]}, "hard_filter_json": {"all": [node]},
                    "version": 1, "created_by": "ingestion:source_codes",
                    "created_at": Utc::now(), "updated_at": Utc::now()
                }))
                .unwrap()
            })
        })
        .collect();
        let reviewed_rules = [
            None,
            Some(
                json!({"all": [{"id": "R1", "field": "employment_status", "op": "ne",
                                 "value": "employed", "origin": "review", "evidence": "x"}]}),
            ),
            Some(
                json!({"all": [{"id": "R1", "field": "household_size", "op": "lte",
                                 "value": 2, "origin": "review", "evidence": "x"}]}),
            ),
        ];
        let mut verified = 0;
        let mut listed = 0;
        for shape in &shapes {
            let prog = program(Uuid::new_v4(), shape);
            for rule_row in &rule_rows {
                for status in ["complete", "incomplete", "conflict", "missing"] {
                    for current in [true, false] {
                        for window_kind in ["open", "past", "rolling", "recurring", "closed"] {
                            for rule in &reviewed_rules {
                                for applicant in [
                                    Some(ApplicantClass::Individual),
                                    Some(ApplicantClass::Indirect),
                                    None,
                                ] {
                                    let review = (status != "missing").then(|| {
                                        ServeReview::from_columns(
                                            status,
                                            rule.as_ref(),
                                            &window(window_kind),
                                            if status == "incomplete" {
                                                vec!["other".into()]
                                            } else {
                                                Vec::new()
                                            },
                                            Some(now - chrono::Duration::days(2)),
                                            current,
                                        )
                                    });
                                    let facts = ReviewFacts { review, applicant };
                                    for profile in &profiles {
                                        let Ok((_, item)) = evaluate_ranked(
                                            &prog,
                                            None,
                                            profile,
                                            None,
                                            rule_row.as_ref(),
                                            &crate::services::recommendations::unreserved(),
                                            Relevance::default(),
                                            ReviewContext {
                                                facts: Some(&facts),
                                                now,
                                                section_enabled: true,
                                            },
                                        ) else {
                                            continue;
                                        };
                                        listed += 1;
                                        if item.verified {
                                            verified += 1;
                                            assert_eq!(item.eligibility_status, "pass", "{item:?}");
                                            assert!(item.missing_checks.is_empty(), "{item:?}");
                                            assert!(item.remaining_checks.is_empty(), "{item:?}");
                                            assert!(item.window.as_ref().unwrap().open_now);
                                            assert!(item.conditions_verified);
                                            // Audit M2: the badge never sits on
                                            // a condition the counts leave open.
                                            assert_eq!(
                                                item.conditions.conditions_unknown, 0,
                                                "{item:?}"
                                            );
                                            assert!(status == "complete" && current);
                                            assert_eq!(applicant, Some(ApplicantClass::Individual));
                                            assert_ne!(prog.program_type, "corporate_benefit");
                                        } else {
                                            assert!(!item.remaining_checks.is_empty(), "{item:?}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(verified > 0 && listed > verified, "{verified}/{listed}");
    }

    // ── Code overrides: the seven independent-check cases ───────────────

    const ALL_SIDOS: [&str; 17] = [
        "busan",
        "chungbuk",
        "chungnam",
        "daegu",
        "daejeon",
        "gangwon",
        "gwangju",
        "gyeongbuk",
        "gyeonggi",
        "gyeongnam",
        "incheon",
        "jeju",
        "jeonbuk",
        "jeonnam",
        "sejong",
        "seoul",
        "ulsan",
    ];

    struct OverrideCase {
        name: &'static str,
        /// Programme columns (see [`program`]).
        columns: Value,
        /// Compiled source conditions (`eligibility_rules`), if any.
        hard: Option<Value>,
        /// Notice text holding the quotes.
        text: &'static str,
        overrides: Value,
        /// Hidden today; listed after the import.
        widened: Value,
        reason: ExclusionReason,
        /// Still hidden after the import (outside the replacement), and why.
        outside: Option<(Value, ExclusionReason)>,
        check: &'static str,
    }

    fn policy(id: Uuid, columns: &Value) -> Program {
        let mut columns = columns.clone();
        columns["program_type"] = json!("policy");
        program(id, &columns)
    }

    fn source_rule(hard: &Value) -> EligibilityRule {
        serde_json::from_value(json!({
            "id": Uuid::new_v4(), "program_id": Uuid::nil(), "rule_json": hard,
            "hard_filter_json": hard, "version": 1, "created_by": "ingestion:source_codes",
            "created_at": Utc::now(), "updated_at": Utc::now()
        }))
        .unwrap()
    }

    fn person(age: i32, extra: Value) -> ProfileInput {
        let mut base = json!({"birth_year": mazimi_core::seoul_year(Utc::now()) - age});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    fn override_cases() -> Vec<OverrideCase> {
        let job = json!({"field": "employment_status", "op": "in",
                         "value": ["preparing", "student", "unemployed"],
                         "origin": "source_codes", "weight": 20});
        let chungbuk = [
            "43111", "43112", "43113", "43114", "43130", "43150", "43720", "43730", "43740",
            "43745", "43750", "43760", "43770", "43800",
        ];
        let near_chungbuk = json!({"field": "region_code", "op": "in", "value":
            ["chungbuk", "gyeonggi", "gangwon", "chungnam", "gyeongbuk", "daejeon", "sejong", "jeonbuk"]});
        vec![
            OverrideCase {
                name: "20250122005400110346 청년도전지원사업 (군 경력자 39세)",
                columns: json!({"min_age": 18, "max_age": 34, "regions": ALL_SIDOS}),
                hard: Some(json!({"all": [job]})),
                text: "※ 군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)",
                overrides: json!([{"target": "S-age", "action": "widen",
                    "replacement": {"field": "age", "op": "between", "value": [18, 39]},
                    "check_kind": "military_service",
                    "evidence": "군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)"}]),
                widened: json!({"region_code": "busan", "employment_status": "unemployed", "age": 37}),
                reason: ExclusionReason::Age,
                outside: Some((
                    json!({"region_code": "busan", "employment_status": "unemployed", "age": 41}),
                    ExclusionReason::Age,
                )),
                check: "military_service",
            },
            OverrideCase {
                name: "20260506005400213171 (부평구) 청년도전 (35~39세)",
                columns: json!({"min_age": 18, "max_age": 34, "regions": ["incheon"],
                                "region_districts": ["28237"]}),
                hard: Some(json!({"all": [job]})),
                text: "지원연령: 부평구 거주자 35~39세 참여가능",
                overrides: json!([{"target": "S-age", "action": "widen",
                    "replacement": {"field": "age", "op": "between", "value": [18, 39]},
                    "check_kind": "other", "evidence": "부평구 거주자 35~39세 참여가능"}]),
                widened: json!({"region_code": "incheon", "city_code": "28237",
                                "employment_status": "unemployed", "age": 37}),
                reason: ExclusionReason::Age,
                // The district limit still applies: 계양구 is not 부평구. (The
                // retired 중구 28110 is now a missing city_code, review R-M4.)
                outside: Some((
                    json!({"region_code": "incheon", "city_code": "28245",
                           "employment_status": "unemployed", "age": 37}),
                    ExclusionReason::Region,
                )),
                check: "other",
            },
            OverrideCase {
                name: "20260710005400113256 국민취업지원제도 (불완전 취업자)",
                columns: json!({"min_age": 15, "max_age": 69, "regions": ALL_SIDOS}),
                hard: Some(json!({"all": [job]})),
                text: "참여제한대상 : 현재 취업 중인 자 (단, 불완전 취업자는 예외)",
                overrides: json!([{"target": "C-job", "action": "drop",
                    "check_kind": "employer_or_job_detail",
                    "evidence": "현재 취업 중인 자 (단, 불완전 취업자는 예외)"}]),
                widened: json!({"region_code": "seoul", "employment_status": "employed", "age": 30}),
                reason: ExclusionReason::HardRule,
                outside: None,
                check: "employer_or_job_detail",
            },
            OverrideCase {
                name: "20260930005400213702 충북 (인접 시도 가능)",
                columns: json!({"min_age": 20, "max_age": 75, "regions": ["chungbuk"],
                                "region_districts": chungbuk}),
                hard: None,
                text: "도내(인접 시도 가능) 소재 기업 재직자",
                overrides: json!([
                    {"target": "S-region", "action": "widen", "replacement": near_chungbuk,
                     "check_kind": "target_group_other", "evidence": "도내(인접 시도 가능)"},
                    {"target": "S-district", "action": "widen", "replacement": near_chungbuk,
                     "check_kind": "target_group_other", "evidence": "도내(인접 시도 가능)"}]),
                widened: json!({"region_code": "gyeonggi", "age": 30}),
                reason: ExclusionReason::Region,
                outside: Some((json!({"region_code": "seoul", "age": 30}), ExclusionReason::Region)),
                check: "target_group_other",
            },
            OverrideCase {
                name: "20260313005400212142 용인 (거주자 또는 관내 대학 재학)",
                columns: json!({"min_age": 18, "max_age": 39, "regions": ["gyeonggi"],
                                "region_districts": ["41461", "41463", "41465"]}),
                hard: Some(json!({"all": [job]})),
                text: "용인시 거주자 또는 관내 대학 재(휴학)중인 구직자",
                overrides: json!([{"target": "S-district", "action": "widen",
                    "replacement": {"any": [
                        {"field": "city_code", "op": "in", "value": ["41461", "41463", "41465"]},
                        {"field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"]}]},
                    "check_kind": "school_detail",
                    "evidence": "용인시 거주자 또는 관내 대학 재(휴학)중인 구직자"}]),
                widened: json!({"region_code": "gyeonggi", "city_code": "41110",
                                "enrollment_status": "enrolled", "employment_status": "student",
                                "age": 22}),
                reason: ExclusionReason::Region,
                outside: Some((
                    json!({"region_code": "gyeonggi", "city_code": "41110",
                           "enrollment_status": "graduated",
                           "employment_status": "unemployed", "age": 25}),
                    ExclusionReason::Region,
                )),
                check: "school_detail",
            },
            OverrideCase {
                name: "20261002005400213808 대학일자리플러스센터 (재학생 및 지역 청년)",
                columns: json!({"min_age": 19, "max_age": 39, "regions": ALL_SIDOS,
                                "enrollment_statuses": ["enrolled", "on_leave"]}),
                hard: Some(json!({"all": [{"field": "education_stage", "op": "in",
                    "value": ["univ_enrolled", "univ_expected"], "origin": "source_codes",
                    "weight": 20}]})),
                text: "고려대학교 세종캠퍼스 재학생 및 지역 청년을 대상으로",
                overrides: json!([
                    {"target": "C-school", "action": "drop", "check_kind": "target_group_other",
                     "evidence": "재학생 및 지역 청년을 대상으로"},
                    {"target": "S-enrollment", "action": "drop", "check_kind": "target_group_other",
                     "evidence": "재학생 및 지역 청년을 대상으로"}]),
                widened: json!({"region_code": "sejong", "school_type": "university",
                                "enrollment_status": "graduated", "age": 27}),
                reason: ExclusionReason::Enrollment,
                outside: None,
                check: "target_group_other",
            },
            OverrideCase {
                name: "20260320005400112237 재직자 AI·D 집중과정 (30세 이상 우선)",
                columns: json!({"min_age": 29, "max_age": 100, "regions": ALL_SIDOS}),
                hard: None,
                text: "30세 이상 성인 재직자를 우선 선발하되, 운영기관별 상황에 따라 이직을 위한 구직자 등 선발 가능",
                overrides: json!([{"target": "S-age", "action": "drop", "check_kind": "other",
                    "evidence": "30세 이상 성인 재직자를 우선 선발하되"}]),
                widened: json!({"region_code": "busan", "employment_status": "employed", "age": 24}),
                reason: ExclusionReason::Age,
                outside: None,
                check: "other",
            },
        ]
    }

    /// Before the import the profile is hidden by the code rule; after it
    /// the row is listed with the override's check and never verified; a
    /// changed text (stale hash) hides it again. Outside the replacement
    /// the row stays hidden for the same reason.
    #[test]
    fn code_overrides_list_what_the_text_admits_until_the_text_changes() {
        let now = noon("2026-10-15");
        for case in override_cases() {
            let id = Uuid::new_v4();
            let prog = policy(id, &case.columns);
            let rule = case.hard.as_ref().map(source_rule);
            let raw = json!({"plcyNm": case.name, "addAplyQlfcCndCn": case.text,
                             "plcyAplyMthdCn": "온라인 신청"});
            let input: store::ReviewInput = serde_json::from_value(json!({
                "source_type": "youth_center", "source_id": id.to_string(),
                "text_hash": condition_review::text_hash("youth_center", &raw).unwrap(),
                "status": "complete", "rules": [],
                "window": {"kind": "rolling", "evidence": case.text},
                "coverage": covered(&raw),
                "reviewer": "test", "code_overrides": case.overrides
            }))
            .unwrap();
            let columns = condition_review::overrides::CodeColumns::of(
                &prog,
                rule.as_ref().and_then(|r| r.hard_filter_json.as_ref()),
            );
            let valid = store::validate_with(&input, &raw, true, &columns)
                .unwrap_or_else(|e| panic!("{}: {e:?}", case.name));
            let facts = |current: bool, overrides: bool| ReviewFacts {
                review: Some(
                    ServeReview::from_columns(
                        valid.status.as_str(),
                        valid.rule_json.as_ref(),
                        &valid.window_json,
                        valid.remaining_checks.clone(),
                        Some(now - chrono::Duration::days(1)),
                        current,
                    )
                    .with_code_overrides(if overrides {
                        valid.code_overrides.as_ref()
                    } else {
                        None
                    }),
                ),
                applicant: Some(ApplicantClass::Individual),
            };
            let run = |who: &Value, facts: Option<&ReviewFacts>| {
                let mut who = who.clone();
                let age = who.as_object_mut().unwrap().remove("age").unwrap();
                evaluate_ranked(
                    &prog,
                    None,
                    &person(age.as_i64().unwrap() as i32, who),
                    None,
                    rule.as_ref(),
                    &crate::services::recommendations::unreserved(),
                    Relevance::default(),
                    ReviewContext {
                        facts,
                        now,
                        section_enabled: true,
                    },
                )
            };
            let name = case.name;
            assert_eq!(
                run(&case.widened, None).err(),
                Some(case.reason),
                "{name}: before"
            );
            let reviewed = facts(true, true);
            let item = run(&case.widened, Some(&reviewed))
                .unwrap_or_else(|e| panic!("{name}: still hidden {e:?}"))
                .1;
            assert!(!item.verified, "{name}");
            assert_ne!(item.eligibility_status, "pass", "{name}");
            assert!(
                item.remaining_checks.iter().any(|c| c.kind == case.check),
                "{name}: {:?}",
                item.remaining_checks
            );
            assert_eq!(
                run(&case.widened, Some(&facts(false, true))).err(),
                Some(case.reason),
                "{name}: stale review"
            );
            if let Some((outside, reason)) = &case.outside {
                assert_eq!(
                    run(outside, Some(&reviewed)).err(),
                    Some(*reason),
                    "{name}: outside the replacement"
                );
            }
        }
    }

    /// The same review without its override verifies an in-range profile;
    /// with it, never (the override leaves a check open).
    #[test]
    fn an_override_withholds_verified_from_profiles_the_code_admits() {
        let now = noon("2026-10-15");
        let case = &override_cases()[0];
        let prog = policy(Uuid::new_v4(), &case.columns);
        let rule = source_rule(case.hard.as_ref().unwrap());
        let review = |overrides: Option<&Value>| ReviewFacts {
            review: Some(
                ServeReview::from_columns(
                    "complete",
                    None,
                    &json!({"kind": "rolling", "evidence": case.text}),
                    Vec::new(),
                    Some(now - chrono::Duration::days(1)),
                    true,
                )
                .with_code_overrides(overrides),
            ),
            applicant: Some(ApplicantClass::Individual),
        };
        let stored: Value = json!([{"target": "S-age", "action": "widen",
            "replacement": {"field": "age", "op": "between", "value": [18, 39]},
            "check_kind": "military_service", "evidence": "x"}]);
        let young = person(
            25,
            json!({"region_code": "busan", "employment_status": "unemployed"}),
        );
        let run = |facts: &ReviewFacts| {
            evaluate_ranked(
                &prog,
                None,
                &young,
                None,
                Some(&rule),
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts: Some(facts),
                    now,
                    section_enabled: true,
                },
            )
            .unwrap()
            .1
        };
        assert!(run(&review(None)).verified);
        let item = run(&review(Some(&stored)));
        assert!(!item.verified);
        assert_eq!(item.remaining_checks[0].kind, "military_service");
        assert_eq!(item.missing_checks[0], "병역 조건 확인이 필요해요");
    }

    /// Review of 165ccbf1: an override's replacement the profile cannot
    /// answer leaves the row unconfirmed, even when the rule left after the
    /// stripping passes. Not "conditions verified", so no rule tier.
    #[test]
    fn an_unknown_override_replacement_is_not_conditions_verified() {
        let now = noon("2026-10-15");
        let job = json!({"field": "employment_status", "op": "in",
                         "value": ["preparing", "student", "unemployed"],
                         "origin": "source_codes", "weight": 20});
        let mut rule = source_rule(&json!({"all": [job]}));
        rule.rule_json = json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"}, job]});
        rule.created_by = None;
        let prog = policy(Uuid::new_v4(), &json!({"regions": ALL_SIDOS}));
        let no_job = person(30, json!({"region_code": "busan"}));
        let facts = ReviewFacts {
            review: Some(
                ServeReview::from_columns(
                    "incomplete",
                    None,
                    &json!({"kind": "unknown"}),
                    vec!["other".into()],
                    None,
                    true,
                )
                .with_code_overrides(Some(
                    &json!([{"target": "C-job", "action": "widen",
                    "replacement": {"field": "employment_status", "op": "in",
                                    "value": ["preparing", "student", "unemployed", "employed"]},
                    "check_kind": "employer_or_job_detail", "evidence": "x"}]),
                )),
            ),
            applicant: Some(ApplicantClass::Individual),
        };
        let item = evaluate_ranked(
            &prog,
            None,
            &no_job,
            None,
            Some(&rule),
            &crate::services::recommendations::unreserved(),
            Relevance::default(),
            ReviewContext {
                facts: Some(&facts),
                now,
                section_enabled: false,
            },
        )
        .expect("listed")
        .1;
        assert!(!item.verified, "{item:?}");
        assert!(!item.conditions_verified, "{item:?}");
    }

    /// Re-review H1: dropping a `C-` target that was the whole hard filter
    /// must not hand the row to the legacy gate, which would hide it on the
    /// soft ranking conditions left (income) although it is listed today.
    #[test]
    fn a_dropped_hard_filter_does_not_turn_soft_conditions_into_a_hide() {
        let now = noon("2026-10-15");
        let job = json!({"field": "employment_status", "op": "in",
                         "value": ["preparing", "student", "unemployed"],
                         "origin": "source_codes", "weight": 20});
        let mut rule = source_rule(&json!({"all": [job]}));
        rule.rule_json = json!({"all": [
            {"field": "income_bracket", "op": "lte", "value": 5}, job]});
        let prog = policy(Uuid::new_v4(), &json!({"regions": ALL_SIDOS}));
        let who = person(
            30,
            json!({"region_code": "busan", "employment_status": "unemployed",
                   "income_bracket": 9}),
        );
        let facts = ReviewFacts {
            review: Some(
                ServeReview::from_columns(
                    "incomplete",
                    None,
                    &json!({"kind": "unknown"}),
                    vec!["other".into()],
                    None,
                    true,
                )
                .with_code_overrides(Some(&json!([{"target": "C-job", "action": "drop",
                    "check_kind": "employer_or_job_detail", "evidence": "x"}]))),
            ),
            applicant: Some(ApplicantClass::Individual),
        };
        let run = |facts: Option<&ReviewFacts>| {
            evaluate_ranked(
                &prog,
                None,
                &who,
                None,
                Some(&rule),
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts,
                    now,
                    section_enabled: true,
                },
            )
        };
        let before = run(None).expect("listed today").1;
        assert_eq!(before.eligibility_status, "unknown");
        let after = run(Some(&facts)).expect("still listed after the drop").1;
        assert!(!after.verified);
        assert!(after
            .remaining_checks
            .iter()
            .any(|c| c.kind == "employer_or_job_detail"));
    }

    /// Review 2026-10-21 M1: a review drops the job code rule; an employed
    /// user passes the region rule left after the stripping. The row is
    /// listed only through the override, so it is not "conditions verified"
    /// (no rule tier) although the leftover rule passes.
    #[test]
    fn an_override_admitted_row_is_never_conditions_verified() {
        let now = noon("2026-10-15");
        let job = json!({"field": "employment_status", "op": "in",
                         "value": ["preparing", "student", "unemployed"],
                         "origin": "source_codes", "weight": 20});
        let mut rule = source_rule(&json!({"all": [job]}));
        rule.rule_json = json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"}, job]});
        rule.created_by = None;
        let prog = policy(Uuid::new_v4(), &json!({"regions": ALL_SIDOS}));
        let employed = person(
            30,
            json!({"region_code": "busan", "employment_status": "employed"}),
        );
        let facts = ReviewFacts {
            review: Some(
                ServeReview::from_columns(
                    "incomplete",
                    None,
                    &json!({"kind": "unknown"}),
                    vec!["other".into()],
                    None,
                    true,
                )
                .with_code_overrides(Some(&json!([{"target": "C-job", "action": "drop",
                    "check_kind": "employer_or_job_detail", "evidence": "x"}]))),
            ),
            applicant: Some(ApplicantClass::Individual),
        };
        // With the verified section on too: the section must not turn an
        // override-admitted row into a verified one.
        for section_enabled in [false, true] {
            let run = |facts: Option<&ReviewFacts>| {
                evaluate_ranked(
                    &prog,
                    None,
                    &employed,
                    None,
                    Some(&rule),
                    &crate::services::recommendations::unreserved(),
                    Relevance::default(),
                    ReviewContext {
                        facts,
                        now,
                        section_enabled,
                    },
                )
            };
            assert_eq!(run(None).err(), Some(ExclusionReason::HardRule));
            let item = run(Some(&facts)).expect("listed through the drop").1;
            assert!(!item.verified, "{section_enabled} {item:?}");
            assert!(!item.conditions_verified, "{section_enabled} {item:?}");
            assert_eq!(item.eligibility_status, "unknown", "{section_enabled}");
        }
    }

    /// Audit H1: a `complete` row written before the coverage check (no
    /// `coverage_version`) is served incomplete; the same row re-written
    /// through the check is served complete.
    #[tokio::test]
    async fn a_complete_row_the_coverage_check_never_saw_is_served_incomplete() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let id = Uuid::new_v4();
        let source_id = format!("cov-{}", id.simple());
        let raw = json!({"plcyNm": "청년 구직 지원", "addAplyQlfcCndCn": "부산 거주 미취업 청년",
                         "plcyAplyMthdCn": "온라인 신청"});
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,raw_payload)
                     VALUES($1,'policy','youth_center',$2,'청년 구직 지원','active',true,$3)")
            .bind(id).bind(&source_id).bind(&raw).execute(&pool).await.unwrap();
        let hash = condition_review::text_hash("youth_center", &raw).unwrap();
        sqlx::query("INSERT INTO program_condition_reviews(source_type,source_id,status,rule_json,window_json,remaining_checks,coverage_json,evidence,text_hash,reviewer)
                     VALUES('youth_center',$1,'complete',NULL,'{\"kind\":\"unknown\"}','{}','{}','[]',$2,'legacy')")
            .bind(&source_id).bind(&hash).execute(&pool).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let status =
            |facts: &HashMap<Uuid, ReviewFacts>| facts[&id].review.as_ref().unwrap().status;
        let legacy = load(&mut conn, &[id]).await.unwrap();
        assert_eq!(status(&legacy), condition_review::ReviewStatus::Incomplete);
        let input: store::ReviewInput = serde_json::from_value(json!({
            "source_type": "youth_center", "source_id": source_id, "text_hash": hash,
            "status": "complete", "window": {"kind": "unknown"}, "reviewer": "test",
            "rules": [{"id": "R1", "field": "employment_status", "op": "in",
                       "value": ["unemployed"], "evidence": "부산 거주 미취업 청년"}],
            "coverage": covered(&raw)
        }))
        .unwrap();
        store::upsert(&mut conn, &input, "import:test", true)
            .await
            .unwrap()
            .unwrap();
        let checked = load(&mut conn, &[id]).await.unwrap();
        assert_eq!(status(&checked), condition_review::ReviewStatus::Complete);
        sqlx::query("DELETE FROM program_condition_reviews WHERE source_id = $1")
            .bind(&source_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// A confirmed rolling window leaves "open now" on the sync that stores
    /// another code, without waiting for the confirmation to expire:
    /// `aplyPrdSeCd` is a hashed field, so the stored review stops being
    /// current and the list reads no window from it.
    #[tokio::test]
    async fn a_rolling_window_leaves_open_now_when_the_synced_code_flips() {
        assert!(
            condition_review::YOUTH_CENTER_CODE_FIELDS.contains(&"aplyPrdSeCd"),
            "availability relies on the code being hashed"
        );
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let source_id = format!(
            "{}",
            80_000_000_000u64 + (id.as_u128() % 1_000_000_000) as u64
        );
        let raw = json!({"plcyNm": "상시 청년 지원 전환", "addAplyQlfcCndCn": "부산 거주 청년",
                         "plcyAplyMthdCn": "온라인 신청", "aplyPrdSeCd": "0057002", "aplyYmd": ""});
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,raw_payload,regions)
                     VALUES($1,'policy','youth_center',$2,'상시 청년 지원 전환','active',true,$3,ARRAY['busan'])")
            .bind(id).bind(&source_id).bind(&raw).execute(&pool).await.unwrap();
        let input: store::ReviewInput = serde_json::from_value(json!({
            "source_type": "youth_center", "source_id": source_id,
            "text_hash": condition_review::text_hash("youth_center", &raw).unwrap(),
            "status": "complete", "rules": [], "window": {"kind": "unknown"},
            "code_rules": [{"id": "S-region", "field": "region_code", "op": "in",
                            "value": ["busan"], "origin": "structural"}],
            "coverage": covered(&raw),
            "remaining_checks": [], "reviewer": "test"
        }))
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        store::upsert(&mut conn, &input, "import:test", true)
            .await
            .unwrap()
            .unwrap();
        // Confirmed rolling a day ago, well inside the TTL.
        let now = Utc::now();
        assert!(store::confirm_window(
            &mut conn,
            "youth_center",
            &source_id,
            input.text_hash.as_deref().unwrap(),
            store::Confirmation::StillRolling,
            now - chrono::Duration::days(1),
            "test",
        )
        .await
        .unwrap());
        let prog: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let profile = person(25, json!({"region_code": "busan"}));
        let open_now = |facts: HashMap<Uuid, ReviewFacts>| {
            let (_, item) = evaluate_ranked(
                &prog,
                None,
                &profile,
                None,
                None,
                &crate::services::recommendations::unreserved(),
                Relevance::default(),
                ReviewContext {
                    facts: facts.get(&id),
                    now,
                    section_enabled: true,
                },
            )
            .unwrap();
            item.window.map(|w| (w.kind, w.open_now))
        };
        let before = load(&mut conn, &[id]).await.unwrap();
        assert!(before[&id].review.as_ref().unwrap().current);
        assert_eq!(
            open_now(before),
            Some((condition_review::WindowKind::Rolling, true))
        );

        // The next sync stores 마감 (0057003).
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"aplyPrdSeCd\":\"0057003\"}' WHERE id=$1")
            .bind(id).execute(&pool).await.unwrap();
        let after = load(&mut conn, &[id]).await.unwrap();
        assert!(
            !after[&id].review.as_ref().unwrap().current,
            "the review is stale"
        );
        assert_eq!(open_now(after), None, "no longer open now");

        sqlx::query("DELETE FROM program_condition_reviews WHERE source_type='youth_center' AND source_id=$1")
            .bind(&source_id).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
