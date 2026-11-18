//! Versioned fact contract (schema v1): what the model may return, the JSON
//! schema it is constrained by, and the normalized fact stored for review.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_DOCUMENTS: usize = 20;
pub const MAX_QUOTE_CHARS: usize = 300;
pub const MIN_QUOTE_CHARS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    Age,
    Region,
    Enrollment,
    Employment,
    Income,
    Document,
    ApplicationWindow,
}

impl FactType {
    pub const ALL: [FactType; 7] = [
        FactType::Age,
        FactType::Region,
        FactType::Enrollment,
        FactType::Employment,
        FactType::Income,
        FactType::Document,
        FactType::ApplicationWindow,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::Region => "region",
            Self::Enrollment => "enrollment",
            Self::Employment => "employment",
            Self::Income => "income",
            Self::Document => "document",
            Self::ApplicationWindow => "application_window",
        }
    }

    /// The property name in the model output object.
    pub fn output_key(self) -> &'static str {
        match self {
            Self::Document => "documents",
            other => other.as_str(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

/// 만나이 is the Korean legal standard and what `programs.min_age/max_age`
/// and `rule_engine::age_gate` mean. Anything else is flagged for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgeBasis {
    /// 만 나이 (international age).
    ManAge,
    /// 연 나이 (current year minus birth year).
    YearAge,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncomeCriterion {
    /// 기준 중위소득 N% (no profile field; recorded, not compiled to a rule).
    MedianIncomePercent,
    /// 소득분위 N (profile `income_bracket`).
    IncomeDecile,
    /// 한국장학재단 학자금 지원구간 N (profile `kosaf_support_bracket`).
    KosafBracket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    Lte,
    Lt,
    Gte,
    Gt,
    Eq,
}

impl Comparator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lte => "lte",
            Self::Lt => "lt",
            Self::Gte => "gte",
            Self::Gt => "gt",
            Self::Eq => "eq",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Dated,
    /// 상시 모집.
    AlwaysOpen,
    /// 예산 소진 시까지.
    UntilBudgetExhausted,
}

/// A validated fact value. Serialized with a `type` tag into
/// `program_ai_extraction_candidates.value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactValue {
    Age {
        min_age: Option<i32>,
        max_age: Option<i32>,
        basis: AgeBasis,
    },
    Region {
        codes: Vec<String>,
    },
    Enrollment {
        statuses: Vec<String>,
    },
    Employment {
        statuses: Vec<String>,
    },
    Income {
        criterion: IncomeCriterion,
        comparator: Comparator,
        value: i32,
    },
    Document {
        name: String,
        required: bool,
        notes: Option<String>,
    },
    ApplicationWindow {
        kind: WindowKind,
        start_date: Option<NaiveDate>,
        end_date: Option<NaiveDate>,
    },
}

impl FactValue {
    pub fn fact_type(&self) -> FactType {
        match self {
            Self::Age { .. } => FactType::Age,
            Self::Region { .. } => FactType::Region,
            Self::Enrollment { .. } => FactType::Enrollment,
            Self::Employment { .. } => FactType::Employment,
            Self::Income { .. } => FactType::Income,
            Self::Document { .. } => FactType::Document,
            Self::ApplicationWindow { .. } => FactType::ApplicationWindow,
        }
    }

    /// Unique per program extraction: one fact per type, one per document.
    pub fn key(&self) -> String {
        match self {
            // Bounded to fit the fact_key column even for an over-long
            // (invalid) name.
            Self::Document { name, .. } => {
                format!(
                    "document:{}",
                    document_key(name).chars().take(100).collect::<String>()
                )
            }
            other => other.fact_type().as_str().to_string(),
        }
    }
}

/// Whitespace- and case-insensitive document identity.
pub fn document_key(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffsetSource {
    /// The model's offset pointed exactly at its quote.
    Model,
    /// The model's offset was wrong but the quote occurs exactly once; the
    /// server located it. Ambiguous quotes are never re-anchored.
    ServerAnchored,
}

/// Evidence for one fact: an exact span of the stored source text.
/// `start`/`end` are char (Unicode scalar) offsets, end exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub quote: String,
    pub start: usize,
    pub end: usize,
    pub source_hash: String,
    pub offset_source: OffsetSource,
}

// ── Model output ──────────────────────────────────────────────────────────

/// Top-level model output. Each section stays raw JSON so one malformed fact
/// is rejected on its own instead of discarding the whole extraction.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawOutput {
    #[serde(default)]
    pub age: Option<Value>,
    #[serde(default)]
    pub region: Option<Value>,
    #[serde(default)]
    pub enrollment: Option<Value>,
    #[serde(default)]
    pub employment: Option<Value>,
    #[serde(default)]
    pub income: Option<Value>,
    #[serde(default)]
    pub documents: Option<Vec<Value>>,
    #[serde(default)]
    pub application_window: Option<Value>,
}

impl RawOutput {
    /// `(type, raw fact)` for every non-null section, documents expanded.
    pub fn sections(&self) -> Vec<(FactType, Value)> {
        let mut out = Vec::new();
        let single = [
            (FactType::Age, &self.age),
            (FactType::Region, &self.region),
            (FactType::Enrollment, &self.enrollment),
            (FactType::Employment, &self.employment),
            (FactType::Income, &self.income),
        ];
        for (fact_type, value) in single {
            if let Some(value) = value.as_ref().filter(|v| !v.is_null()) {
                out.push((fact_type, value.clone()));
            }
        }
        for document in self.documents.iter().flatten() {
            out.push((FactType::Document, document.clone()));
        }
        if let Some(value) = self.application_window.as_ref().filter(|v| !v.is_null()) {
            out.push((FactType::ApplicationWindow, value.clone()));
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceIn {
    pub quote: String,
    #[serde(default)]
    pub start: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgeIn {
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub basis: AgeBasis,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodesIn {
    pub codes: Vec<String>,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusesIn {
    pub statuses: Vec<String>,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomeIn {
    pub criterion: IncomeCriterion,
    pub comparator: Comparator,
    pub value: i32,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIn {
    pub name: String,
    pub required: bool,
    #[serde(default)]
    pub notes: Option<String>,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowIn {
    pub kind: WindowKind,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub confidence: Confidence,
    pub evidence: EvidenceIn,
}

// ── JSON schema (Ollama `format`) ─────────────────────────────────────────

pub const REGION_CODES: &[&str] = &[
    "seoul",
    "busan",
    "daegu",
    "incheon",
    "gwangju",
    "daejeon",
    "ulsan",
    "sejong",
    "gyeonggi",
    "gangwon",
    "chungbuk",
    "chungnam",
    "jeonbuk",
    "jeonnam",
    "gyeongbuk",
    "gyeongnam",
    "jeju",
];
pub const ENROLLMENT_CODES: &[&str] = &["enrolled", "on_leave", "graduated", "research_student"];
/// `other` is deliberately absent: it has no textual basis to verify.
pub const EMPLOYMENT_CODES: &[&str] = &[
    "employed",
    "new_hire",
    "unemployed",
    "preparing",
    "self_employed",
    "student",
];

fn evidence_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["quote", "start"],
        "properties": {
            "quote": {"type": "string", "minLength": MIN_QUOTE_CHARS, "maxLength": MAX_QUOTE_CHARS},
            "start": {"type": "integer", "minimum": 0}
        }
    })
}

fn fact_schema(properties: Value, required: &[&str]) -> Value {
    let mut properties = properties;
    let map = properties.as_object_mut().expect("properties object");
    map.insert(
        "confidence".into(),
        json!({"type": "string", "enum": ["HIGH", "MEDIUM", "LOW"]}),
    );
    map.insert("evidence".into(), evidence_schema());
    let mut required: Vec<&str> = required.to_vec();
    required.extend(["confidence", "evidence"]);
    json!({
        "type": ["object", "null"], "additionalProperties": false,
        "required": required, "properties": properties
    })
}

fn type_schema(fact_type: FactType) -> Value {
    match fact_type {
        FactType::Age => fact_schema(
            json!({
                "min_age": {"type": ["integer", "null"], "minimum": 0, "maximum": 100},
                "max_age": {"type": ["integer", "null"], "minimum": 0, "maximum": 100},
                "basis": {"type": "string", "enum": ["man_age", "year_age", "unspecified"]}
            }),
            &["min_age", "max_age", "basis"],
        ),
        FactType::Region => fact_schema(
            json!({"codes": {"type": "array", "minItems": 1, "maxItems": 17, "uniqueItems": true,
                "items": {"type": "string", "enum": REGION_CODES}}}),
            &["codes"],
        ),
        FactType::Enrollment => fact_schema(
            json!({"statuses": {"type": "array", "minItems": 1, "maxItems": 4, "uniqueItems": true,
                "items": {"type": "string", "enum": ENROLLMENT_CODES}}}),
            &["statuses"],
        ),
        FactType::Employment => fact_schema(
            json!({"statuses": {"type": "array", "minItems": 1, "maxItems": 6, "uniqueItems": true,
                "items": {"type": "string", "enum": EMPLOYMENT_CODES}}}),
            &["statuses"],
        ),
        FactType::Income => fact_schema(
            json!({
                "criterion": {"type": "string", "enum": ["median_income_percent", "income_decile", "kosaf_bracket"]},
                "comparator": {"type": "string", "enum": ["lte", "lt", "gte", "gt", "eq"]},
                "value": {"type": "integer", "minimum": 0, "maximum": 300}
            }),
            &["criterion", "comparator", "value"],
        ),
        FactType::Document => {
            let mut item = fact_schema(
                json!({
                    "name": {"type": "string", "minLength": 1, "maxLength": 100},
                    "required": {"type": "boolean"},
                    "notes": {"type": ["string", "null"], "maxLength": 300}
                }),
                &["name", "required", "notes"],
            );
            item["type"] = json!("object");
            json!({"type": "array", "maxItems": MAX_DOCUMENTS, "items": item})
        }
        FactType::ApplicationWindow => fact_schema(
            json!({
                "kind": {"type": "string", "enum": ["dated", "always_open", "until_budget_exhausted"]},
                "start_date": {"type": ["string", "null"], "format": "date"},
                "end_date": {"type": ["string", "null"], "format": "date"}
            }),
            &["kind", "start_date", "end_date"],
        ),
    }
}

/// Output schema restricted to the requested fact types. Types covered by the
/// rule normalizer are absent, so a schema-following model cannot emit them.
pub fn output_schema(requested: &[FactType]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for fact_type in FactType::ALL {
        if !requested.contains(&fact_type) {
            continue;
        }
        properties.insert(fact_type.output_key().into(), type_schema(fact_type));
        required.push(fact_type.output_key());
    }
    json!({
        "type": "object", "additionalProperties": false,
        "required": required, "properties": properties
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_contains_only_requested_types_and_requires_them() {
        let schema = output_schema(&[FactType::Income, FactType::Document]);
        assert_eq!(schema["additionalProperties"], false);
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties.keys().collect::<Vec<_>>(),
            vec!["documents", "income"]
        );
        assert_eq!(schema["required"], json!(["income", "documents"]));
        assert!(schema["properties"].get("age").is_none());
    }

    #[test]
    fn every_fact_schema_requires_all_its_properties_and_evidence() {
        let schema = output_schema(&FactType::ALL);
        for fact_type in FactType::ALL {
            let mut fact = schema["properties"][fact_type.output_key()].clone();
            if fact_type == FactType::Document {
                fact = fact["items"].clone();
            }
            assert_eq!(fact["additionalProperties"], false, "{fact_type:?}");
            assert_eq!(
                fact["required"].as_array().map(Vec::len),
                fact["properties"].as_object().map(serde_json::Map::len),
                "{fact_type:?}"
            );
            assert_eq!(
                fact["properties"]["evidence"]["required"],
                json!(["quote", "start"])
            );
        }
    }

    #[test]
    fn schema_enums_match_core_canonical_codes() {
        for code in REGION_CODES {
            assert_eq!(mazimi_core::canonical_region_code(code), Some(*code));
        }
        for code in ENROLLMENT_CODES {
            assert_eq!(mazimi_core::canonical_enrollment_status(code), Some(*code));
        }
        for code in EMPLOYMENT_CODES {
            assert_eq!(mazimi_core::canonical_employment_status(code), Some(*code));
        }
    }

    #[test]
    fn fact_values_round_trip_with_a_type_tag_and_stable_keys() {
        let value = FactValue::Document {
            name: "주민등록 등본".into(),
            required: true,
            notes: None,
        };
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["type"], "document");
        assert_eq!(serde_json::from_value::<FactValue>(json).unwrap(), value);
        assert_eq!(value.key(), "document:주민등록등본");
        let window = FactValue::ApplicationWindow {
            kind: WindowKind::Dated,
            start_date: NaiveDate::from_ymd_opt(2026, 3, 1),
            end_date: None,
        };
        assert_eq!(window.key(), "application_window");
        assert_eq!(
            serde_json::to_value(&window).unwrap()["start_date"],
            "2026-03-01"
        );
        assert!(serde_json::from_value::<FactValue>(
            json!({"type":"age","min_age":1,"max_age":2,"basis":"man_age","extra":1})
        )
        .is_err());
    }

    #[test]
    fn raw_output_rejects_unknown_sections_and_expands_documents() {
        assert!(serde_json::from_value::<RawOutput>(json!({"answer": "x"})).is_err());
        let raw: RawOutput = serde_json::from_value(json!({
            "age": null,
            "income": {"criterion": "income_decile"},
            "documents": [{"name": "a"}, {"name": "b"}]
        }))
        .unwrap();
        let sections = raw.sections();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].0, FactType::Income);
        assert_eq!(sections[2].0, FactType::Document);
    }

    #[test]
    fn confidence_orders_and_parses_upper_case() {
        assert!(Confidence::High > Confidence::Medium);
        assert_eq!(
            serde_json::from_value::<Confidence>(json!("HIGH")).unwrap(),
            Confidence::High
        );
        assert!(serde_json::from_value::<Confidence>(json!("high")).is_err());
    }
}
