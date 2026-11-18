//! Phase 0 evaluation: run a provider over a labeled fixture and score
//! per-field precision/recall plus evidence validity. Pure and offline: no
//! database, no `.env`. The fixture format is documented in
//! `docs/validation/ai-extraction/README.md`.
//!
//! The `mock` provider replays the fixture's own labels as a model would
//! (exact quotes, model offsets). It checks the harness and lints the
//! labels (every quote must be verbatim and pass the validator); it is not a
//! quality measurement. Only a real model run against a human-verified
//! fixture is.
use super::crosscheck::cross_check;
use super::facts::{document_key, FactType, FactValue};
use super::provider::{build_request, char_offset, ExtractionProvider, ProviderFailure};
use super::source::{build_source, ProgramText, RuleCoverage};
use super::validate::{process_output, FactState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Instant;

pub const SAMPLE_FIXTURE: &str = include_str!("../../tests/fixtures/ai_extraction_sample.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureMeta {
    pub labeler: String,
    pub created_at: String,
    pub schema_version: i32,
    /// True only after a person checked every label against the source.
    pub human_verified: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFact {
    #[serde(rename = "fact")]
    pub value: FactValue,
    /// Verbatim span of the built source text that states the fact.
    pub quote: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProgram {
    pub id: String,
    #[serde(default)]
    pub origin: Option<String>,
    /// Labeler remark: ambiguity, assumptions, what was left unlabeled.
    #[serde(default)]
    pub label_note: Option<String>,
    pub source: ProgramText,
    /// Fact types the rule normalizer already produced for this program;
    /// they are not requested. Default: nothing covered.
    #[serde(default)]
    pub coverage: RuleCoverage,
    /// Every fact the text states for the requested types. A type with no
    /// expected fact means "absent": any prediction for it is a false
    /// positive.
    pub expected: Vec<ExpectedFact>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub meta: FixtureMeta,
    pub programs: Vec<FixtureProgram>,
}

pub fn parse_fixture(raw: &str) -> Result<Fixture, String> {
    let fixture: Fixture = serde_json::from_str(raw).map_err(|e| format!("fixture: {e}"))?;
    if fixture.programs.is_empty() {
        return Err("fixture: no programs".into());
    }
    let mut ids = std::collections::HashSet::new();
    for program in &fixture.programs {
        if !ids.insert(program.id.clone()) {
            return Err(format!("fixture: duplicate id {}", program.id));
        }
    }
    Ok(fixture)
}

/// The labels in model-output shape with HIGH confidence and exact offsets.
pub fn oracle_response(program: &FixtureProgram, source_text: &str) -> Value {
    let mut out = Map::new();
    let mut documents = Vec::new();
    for fact in &program.expected {
        let mut item = serde_json::to_value(&fact.value).expect("fact serializes");
        let object = item.as_object_mut().expect("tagged object");
        object.remove("type");
        object.insert("confidence".into(), json!("HIGH"));
        object.insert(
            "evidence".into(),
            json!({"quote": fact.quote, "start": char_offset(source_text, &fact.quote).unwrap_or(0)}),
        );
        match fact.value.fact_type() {
            FactType::Document => documents.push(item),
            other => {
                out.insert(other.output_key().into(), item);
            }
        }
    }
    out.insert("documents".into(), Value::Array(documents));
    Value::Object(out)
}

/// Order-insensitive identity used for matching predictions to labels.
fn match_key(value: &FactValue) -> String {
    match value {
        FactValue::Document { name, .. } => format!("document:{}", document_key(name)),
        FactValue::Region { codes } => {
            let mut codes = codes.clone();
            codes.sort();
            format!("region:{}", codes.join(","))
        }
        FactValue::Enrollment { statuses } | FactValue::Employment { statuses } => {
            let mut statuses = statuses.clone();
            statuses.sort();
            format!("{}:{}", value.fact_type().as_str(), statuses.join(","))
        }
        other => serde_json::to_string(other).expect("fact serializes"),
    }
}

/// Stricter identity for the auto-verified subset: what auto-apply writes.
/// A document also needs the right `required` flag, since that is applied
/// without a reviewer.
fn auto_match_key(value: &FactValue) -> String {
    match value {
        FactValue::Document { name, required, .. } => {
            format!("document:{}:{required}", document_key(name))
        }
        other => match_key(other),
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct FieldScore {
    pub tp: usize,
    pub fp: usize,
    #[serde(rename = "fn")]
    pub fn_: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}

impl FieldScore {
    fn finish(mut self) -> Self {
        self.precision =
            (self.tp + self.fp > 0).then(|| self.tp as f64 / (self.tp + self.fp) as f64);
        self.recall =
            (self.tp + self.fn_ > 0).then(|| self.tp as f64 / (self.tp + self.fn_) as f64);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramResult {
    pub id: String,
    pub latency_ms: u128,
    pub error: Option<String>,
    pub returned: usize,
    pub valid: usize,
    pub evidence_valid: usize,
    pub false_positives: Vec<String>,
    pub false_negatives: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub meta: Value,
    pub fields: BTreeMap<String, FieldScore>,
    pub overall: FieldScore,
    /// The auto-verified subset per type: facts in the default queue state
    /// whose quote the rule parser reads identically (what auto-apply would
    /// write). `precision` is the auto-apply gate; `recall` here is the share
    /// of labeled facts that would be applied without a human.
    pub auto_verified: BTreeMap<String, FieldScore>,
    /// Facts whose quote is an exact span of the source / facts returned.
    pub evidence_validity_rate: Option<f64>,
    /// Facts that passed every check (pending or flagged) / facts returned.
    pub valid_rate: Option<f64>,
    pub provider_failures: usize,
    pub latency_ms: Value,
    pub programs: Vec<ProgramResult>,
}

fn percentile(sorted: &[u128], p: f64) -> Option<u128> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

pub async fn run<P: ExtractionProvider + ?Sized>(
    fixture: &Fixture,
    provider: &P,
    oracle: bool,
) -> Report {
    let mut fields: BTreeMap<String, FieldScore> = FactType::ALL
        .iter()
        .map(|t| (t.as_str().to_string(), FieldScore::default()))
        .collect();
    let mut auto_fields = fields.clone();
    let (mut returned, mut evidence_valid, mut valid, mut failures) = (0, 0, 0, 0);
    let mut latencies = Vec::new();
    let mut programs = Vec::new();
    for program in &fixture.programs {
        let source = build_source(&program.source);
        let requested = program.coverage.requested();
        let request = build_request(
            &source,
            &requested,
            provider.provider_name(),
            provider.model_name(),
        );
        let started = Instant::now();
        let response = if oracle {
            Ok(oracle_response(program, &source.text))
        } else {
            provider.extract(&request).await
        };
        let latency = started.elapsed().as_millis();
        latencies.push(latency);
        let mut result = ProgramResult {
            id: program.id.clone(),
            latency_ms: latency,
            error: None,
            returned: 0,
            valid: 0,
            evidence_valid: 0,
            false_positives: vec![],
            false_negatives: vec![],
        };
        let processed = response.and_then(|value| {
            process_output(&value, &source, &program.coverage)
                .map_err(ProviderFailure::InvalidOutput)
        });
        let mut auto_verified: Vec<FactValue> = Vec::new();
        let predicted: Vec<FactValue> = match processed {
            Ok(processed) => {
                result.returned = processed.facts.len();
                for fact in &processed.facts {
                    if fact.evidence.is_some() && !fact.validation.evidence_rejected() {
                        result.evidence_valid += 1;
                    }
                    if let (FactState::Pending, Some(value), Some(evidence)) =
                        (fact.state, fact.fact_value(), fact.evidence.as_ref())
                    {
                        if cross_check(&value, &evidence.quote).agrees() {
                            auto_verified.push(value);
                        }
                    }
                }
                processed
                    .facts
                    .iter()
                    .filter(|f| f.state != FactState::Invalid)
                    .filter_map(|f| f.fact_value())
                    .collect()
            }
            Err(error) => {
                failures += 1;
                result.error = Some(error.to_string());
                Vec::new()
            }
        };
        result.valid = predicted.len();
        returned += result.returned;
        evidence_valid += result.evidence_valid;
        valid += result.valid;
        let expected: Vec<&FactValue> = program
            .expected
            .iter()
            .map(|e| &e.value)
            .filter(|v| requested.contains(&v.fact_type()))
            .collect();
        let expected_keys: Vec<String> = expected.iter().map(|v| match_key(v)).collect();
        let predicted_keys: Vec<String> = predicted.iter().map(match_key).collect();
        for (value, key) in predicted.iter().zip(&predicted_keys) {
            let score = fields
                .get_mut(value.fact_type().as_str())
                .expect("all types");
            if expected_keys.contains(key) {
                score.tp += 1;
            } else {
                score.fp += 1;
                result.false_positives.push(key.clone());
            }
        }
        for (value, key) in expected.iter().zip(&expected_keys) {
            if !predicted_keys.contains(key) {
                fields
                    .get_mut(value.fact_type().as_str())
                    .expect("all types")
                    .fn_ += 1;
                result.false_negatives.push(key.clone());
            }
        }
        let expected_auto: Vec<String> = expected.iter().map(|v| auto_match_key(v)).collect();
        let auto_keys: Vec<String> = auto_verified.iter().map(auto_match_key).collect();
        for (value, key) in auto_verified.iter().zip(&auto_keys) {
            let score = auto_fields
                .get_mut(value.fact_type().as_str())
                .expect("all types");
            if expected_auto.contains(key) {
                score.tp += 1;
            } else {
                score.fp += 1;
            }
        }
        for (value, key) in expected.iter().zip(&expected_auto) {
            if !auto_keys.contains(key) {
                auto_fields
                    .get_mut(value.fact_type().as_str())
                    .expect("all types")
                    .fn_ += 1;
            }
        }
        programs.push(result);
    }
    for score in auto_fields.values_mut() {
        *score = score.finish();
    }
    let mut overall = FieldScore::default();
    for score in fields.values_mut() {
        overall.tp += score.tp;
        overall.fp += score.fp;
        overall.fn_ += score.fn_;
        *score = score.finish();
    }
    latencies.sort_unstable();
    let ratio = |n: usize, d: usize| (d > 0).then(|| n as f64 / d as f64);
    Report {
        meta: json!({
            "labeler": fixture.meta.labeler,
            "human_verified": fixture.meta.human_verified,
            "fixture_created_at": fixture.meta.created_at,
            "programs": fixture.programs.len(),
            "provider": if oracle { "mock-oracle" } else { provider.provider_name() },
            "model": provider.model_name(),
            "prompt_version": super::PROMPT_VERSION,
            "schema_version": super::SCHEMA_VERSION,
        }),
        fields,
        overall: overall.finish(),
        auto_verified: auto_fields,
        evidence_validity_rate: ratio(evidence_valid, returned),
        valid_rate: ratio(valid, returned),
        provider_failures: failures,
        latency_ms: json!({
            "p50": percentile(&latencies, 0.5),
            "p95": percentile(&latencies, 0.95),
            "max": latencies.last(),
        }),
        programs,
    }
}

/// Rollout gate: `None` thresholds are not checked. Returns the failures.
pub fn gate(
    report: &Report,
    min_precision: f64,
    min_recall: f64,
    min_evidence_validity: f64,
) -> Vec<String> {
    let mut failed = Vec::new();
    if report.overall.precision.unwrap_or(0.0) < min_precision {
        failed.push(format!("overall precision < {min_precision}"));
    }
    if report.overall.recall.unwrap_or(0.0) < min_recall {
        failed.push(format!("overall recall < {min_recall}"));
    }
    if report.evidence_validity_rate.unwrap_or(0.0) < min_evidence_validity {
        failed.push(format!("evidence validity < {min_evidence_validity}"));
    }
    if report.provider_failures > 0 {
        failed.push(format!("{} provider failures", report.provider_failures));
    }
    failed
}

/// Auto-apply gate per fact type: the auto-verified subset needs at least
/// `min_support` facts and precision ≥ `min_precision` on the human-verified
/// label set before that type may be listed in EXTRACTION_AUTO_APPLY_TYPES.
/// Returns the failures.
pub fn auto_gate(
    report: &Report,
    types: &[FactType],
    min_precision: f64,
    min_support: usize,
) -> Vec<String> {
    let mut failed = Vec::new();
    for fact_type in types {
        let score = report
            .auto_verified
            .get(fact_type.as_str())
            .copied()
            .unwrap_or_default();
        let support = score.tp + score.fp;
        if support < min_support {
            failed.push(format!(
                "{}: {support} auto-verified facts < {min_support}",
                fact_type.as_str()
            ));
        } else if score.precision.unwrap_or(0.0) < min_precision {
            failed.push(format!(
                "{}: auto-verified precision {:.3} < {min_precision}",
                fact_type.as_str(),
                score.precision.unwrap_or(0.0)
            ));
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::super::provider::MockExtractionProvider;
    use super::*;

    #[tokio::test]
    async fn sample_fixture_labels_are_verbatim_and_pass_the_validator() {
        let fixture = parse_fixture(SAMPLE_FIXTURE).unwrap();
        assert!(fixture.programs.len() >= 10);
        assert!(!fixture.meta.human_verified, "agent draft must say so");
        for program in &fixture.programs {
            let source = build_source(&program.source);
            for fact in &program.expected {
                assert!(
                    source.text.contains(&fact.quote),
                    "{}: quote {:?} not in source\n{}",
                    program.id,
                    fact.quote,
                    source.text
                );
            }
        }
        let provider = MockExtractionProvider::new("oracle", |_| Ok(json!({})));
        let report = run(&fixture, &provider, true).await;
        assert_eq!(report.overall.fp, 0, "{:#?}", report.programs);
        assert_eq!(report.overall.fn_, 0, "{:#?}", report.programs);
        assert_eq!(report.evidence_validity_rate, Some(1.0));
        assert!(gate(&report, 1.0, 1.0, 1.0).is_empty());
    }

    #[tokio::test]
    async fn scoring_counts_false_positives_negatives_and_invalid_evidence() {
        let fixture = parse_fixture(SAMPLE_FIXTURE).unwrap();
        let one = Fixture {
            meta: fixture.meta.clone(),
            programs: vec![fixture.programs[0].clone()],
        };
        // A model that returns nothing has perfect "precision" on no output
        // but zero recall; one that invents a quote has invalid evidence.
        let silent = MockExtractionProvider::new("silent", |_| Ok(json!({})));
        let report = run(&one, &silent, false).await;
        assert_eq!(report.overall.tp, 0);
        assert!(report.overall.fn_ > 0);
        assert_eq!(report.overall.recall, Some(0.0));
        assert!(!gate(&report, 0.9, 0.5, 0.9).is_empty());

        let inventive = MockExtractionProvider::new("inventive", |_| {
            Ok(
                json!({"income": {"criterion": "income_decile", "comparator": "lte", "value": 3,
                "confidence": "HIGH", "evidence": {"quote": "소득 3분위 이하", "start": 0}}}),
            )
        });
        let report = run(&one, &inventive, false).await;
        assert_eq!(report.evidence_validity_rate, Some(0.0));
        assert_eq!(report.overall.tp, 0);

        let down = MockExtractionProvider::new("down", |_| Err(ProviderFailure::Unavailable));
        let report = run(&one, &down, false).await;
        assert_eq!(report.provider_failures, 1);
        assert!(gate(&report, 0.0, 0.0, 0.0)
            .iter()
            .any(|g| g.contains("provider failures")));
    }

    #[tokio::test]
    async fn auto_verified_subset_is_scored_and_gated_per_type() {
        let fixture = parse_fixture(SAMPLE_FIXTURE).unwrap();
        let oracle = MockExtractionProvider::new("oracle", |_| Ok(json!({})));
        let report = run(&fixture, &oracle, true).await;
        // Correct labels replayed: whatever the parser verifies is right.
        for (field, score) in &report.auto_verified {
            assert_eq!(score.fp, 0, "{field}: {score:?}");
        }
        let documents = report.auto_verified["document"];
        assert!(documents.tp > 0, "{:#?}", report.auto_verified);
        assert!(auto_gate(&report, &[FactType::Document], 0.98, 1).is_empty());
        assert!(
            !auto_gate(&report, &[FactType::Document], 0.98, 10_000).is_empty(),
            "support below the minimum fails the gate"
        );

        // A labeler says this sentence is not an eligibility age (it is a
        // program for parents); the model's fact is rule-verified anyway, so
        // it counts against auto-apply precision.
        let text = "만 19세~34세 자녀를 둔 부모";
        let one = Fixture {
            meta: fixture.meta.clone(),
            programs: vec![FixtureProgram {
                id: "parents".into(),
                origin: None,
                label_note: None,
                source: ProgramText {
                    title: "부모 지원".into(),
                    summary: Some(text.into()),
                    ..ProgramText::default()
                },
                coverage: RuleCoverage::default(),
                expected: vec![],
            }],
        };
        let model = MockExtractionProvider::new("model", |request| {
            let start = char_offset(&request.source_text, "만 19세~34세").unwrap();
            Ok(
                json!({"age": {"min_age": 19, "max_age": 34, "basis": "man_age", "confidence": "HIGH",
                "evidence": {"quote": "만 19세~34세", "start": start}}}),
            )
        });
        let report = run(&one, &model, false).await;
        assert_eq!(report.auto_verified["age"].fp, 1);
        assert_eq!(report.auto_verified["age"].precision, Some(0.0));
        assert!(!auto_gate(&report, &[FactType::Age], 0.98, 1).is_empty());
        // A fact the parser does not read identically is not in the subset.
        let wrong = MockExtractionProvider::new("wrong", |request| {
            let start = char_offset(&request.source_text, "만 19세~34세").unwrap();
            Ok(
                json!({"age": {"min_age": 34, "max_age": null, "basis": "man_age", "confidence": "HIGH",
                "evidence": {"quote": "만 19세~34세", "start": start}}}),
            )
        });
        let report = run(&one, &wrong, false).await;
        assert_eq!(
            report.auto_verified["age"].tp + report.auto_verified["age"].fp,
            0
        );
        assert_eq!(report.fields["age"].fp, 1, "still a model false positive");
    }

    #[test]
    fn match_keys_ignore_order_and_document_spacing() {
        assert_eq!(
            match_key(&FactValue::Region {
                codes: vec!["seoul".into(), "busan".into()]
            }),
            match_key(&FactValue::Region {
                codes: vec!["busan".into(), "seoul".into()]
            })
        );
        assert_eq!(
            match_key(&FactValue::Document {
                name: "주민등록 등본".into(),
                required: true,
                notes: None
            }),
            match_key(&FactValue::Document {
                name: "주민등록등본".into(),
                required: false,
                notes: Some("x".into())
            })
        );
        assert_eq!(percentile(&[10, 20, 30, 40], 0.5), Some(20));
        assert_eq!(percentile(&[10, 20, 30, 40], 0.95), Some(40));
        assert_eq!(percentile(&[], 0.5), None);
    }

    #[test]
    fn fixtures_reject_duplicates_and_unknown_fields() {
        assert!(parse_fixture(r#"{"meta":{"labeler":"x","created_at":"d","schema_version":1,"human_verified":false},"programs":[]}"#).is_err());
        let program = r#"{"id":"a","source":{"title":"t"},"expected":[]}"#;
        assert!(parse_fixture(&format!(r#"{{"meta":{{"labeler":"x","created_at":"d","schema_version":1,"human_verified":false}},"programs":[{program},{program}]}}"#)).is_err());
        assert!(parse_fixture(r#"{"meta":{"labeler":"x","created_at":"d","schema_version":1,"human_verified":false,"extra":1},"programs":[]}"#).is_err());
    }
}
