//! A person's reviewed verdict on who applies for a programme, stored in
//! `program_applicant_reviews` and applied when a recommendation list is
//! served. The text rules in [`crate::audience`] stay as they are; a review
//! can only hide or demote what they let through, never list what they hide.
//!
//! A review is bound to the source text the reviewers read through
//! [`text_hash`]: an import line carries the hash of the text the curators
//! read and is rejected when the current text hashes differently. When the
//! upstream text changes later, the stored hash no longer matches and the
//! review stops applying until someone reviews the new text.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// `raw_payload` fields of a 온통청년 row the reviewers read, in hash order.
pub const YOUTH_CENTER_FIELDS: [&str; 9] = [
    "plcyNm",
    "plcyExplnCn",
    "plcySprtCn",
    "etcMttrCn",
    "addAplyQlfcCndCn",
    "ptcpPrpTrgtCn",
    "plcyAplyMthdCn",
    "sbmsnDcmntCn",
    "srngMthdCn",
];

/// Joins field values before hashing (ASCII unit separator; it does not
/// occur in notice text).
const SEPARATOR: char = '\u{1f}';

/// Shortest quote accepted as evidence; shorter strings ("기업") match
/// almost any notice and prove nothing.
pub const MIN_EVIDENCE_CHARS: usize = 5;
pub const MAX_EVIDENCE_CHARS: usize = 2_000;

/// Shown on a listed `indirect` programme (in `missing_checks`).
pub const INDIRECT_NOTE: &str = "기업·대학 등 참여기관을 통해 참여해요";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicantClass {
    /// A person applies.
    Individual,
    /// An employer, university or consortium applies; young people take
    /// part through it.
    Indirect,
    /// Only institutions, municipalities or associations apply.
    Organisation,
    /// Only businesses apply (employer subsidies, company grants).
    Business,
    /// A portal or newsletter with nothing to apply for.
    InfoOnly,
    /// The reviewers could not decide from the text.
    Unclear,
}

impl ApplicantClass {
    pub const ALL: [Self; 6] = [
        Self::Individual,
        Self::Indirect,
        Self::Organisation,
        Self::Business,
        Self::InfoOnly,
        Self::Unclear,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Individual => "individual",
            Self::Indirect => "indirect",
            Self::Organisation => "organisation",
            Self::Business => "business",
            Self::InfoOnly => "info_only",
            Self::Unclear => "unclear",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == value)
    }
}

/// What a review does to a list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEffect {
    /// No change.
    None,
    /// Excluded as not applicable by an individual.
    Hide,
    /// Listed below otherwise-equal rows with [`INDIRECT_NOTE`].
    Demote,
}

/// The effect of a stored review on the current text. A review whose hash
/// differs from the current text's (`None` = the source is not reviewable)
/// is stale and has no effect.
pub fn effect(
    class: ApplicantClass,
    stored_hash: &str,
    current_hash: Option<&str>,
) -> ReviewEffect {
    if current_hash != Some(stored_hash) {
        return ReviewEffect::None;
    }
    match class {
        ApplicantClass::Organisation | ApplicantClass::Business => ReviewEffect::Hide,
        ApplicantClass::Indirect => ReviewEffect::Demote,
        ApplicantClass::Individual | ApplicantClass::InfoOnly | ApplicantClass::Unclear => {
            ReviewEffect::None
        }
    }
}

/// The reviewed fields of a source, `None` when the source has no review
/// field set yet.
pub fn fields(source_type: &str) -> Option<&'static [&'static str]> {
    match source_type {
        "youth_center" => Some(&YOUTH_CENTER_FIELDS),
        _ => None,
    }
}

/// The reviewed field texts of `raw_payload`, in [`fields`] order. A missing
/// or `null` field is empty; a non-string value is its JSON text.
pub fn reviewed_texts(source_type: &str, raw_payload: &Value) -> Option<Vec<String>> {
    Some(
        fields(source_type)?
            .iter()
            .map(|field| field_text(raw_payload, field))
            .collect(),
    )
}

/// One `raw_payload` field as hashed text: missing or `null` is empty, a
/// string is itself, any other value is its JSON text.
pub fn field_text(raw_payload: &Value, field: &str) -> String {
    match raw_payload.get(field) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// Lowercase hex SHA-256 of the reviewed fields joined with a unit
/// separator. The importer, the admin route and the list all call this.
pub fn text_hash(source_type: &str, raw_payload: &Value) -> Option<String> {
    let texts = reviewed_texts(source_type, raw_payload)?;
    let joined = texts.join(&SEPARATOR.to_string());
    Some(format!("{:x}", Sha256::digest(joined.as_bytes())))
}

/// Whether `evidence` is a verbatim quote of one reviewed field.
pub fn evidence_found(source_type: &str, raw_payload: &Value, evidence: &str) -> bool {
    let evidence = evidence.trim();
    let length = evidence.chars().count();
    if !(MIN_EVIDENCE_CHARS..=MAX_EVIDENCE_CHARS).contains(&length) {
        return false;
    }
    reviewed_texts(source_type, raw_payload)
        .is_some_and(|texts| texts.iter().any(|text| text.contains(evidence)))
}

/// One review as written by the importer or an admin. Other keys of an
/// import line (`title`, …) are annotations for people and are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    pub source_type: String,
    pub source_id: String,
    pub class: String,
    pub evidence: String,
    pub reviewer: String,
    /// [`text_hash`] of the text the reviewer read. When given, a write is
    /// rejected if the current text hashes differently.
    #[serde(default)]
    pub text_hash: Option<String>,
}

/// Why a review was not written. No variant carries source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    UnknownClass,
    InvalidReviewer,
    InvalidSource,
    MissingTextHash,
    TextChanged,
    UnsupportedSource,
    ProgramNotFound,
    EvidenceNotFound,
}

impl Rejection {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownClass => "class must be one of individual, indirect, organisation, business, info_only, unclear",
            Self::InvalidReviewer => "reviewer must be 1-200 characters without control characters",
            Self::InvalidSource => "source_type must be 1-100 and source_id 1-200 characters",
            Self::MissingTextHash => "text_hash of the text the reviewers read is required",
            Self::TextChanged => "the programme text changed since it was reviewed (text_hash differs); review the current text",
            Self::UnsupportedSource => "this source has no reviewed field set",
            Self::ProgramNotFound => "no programme with this source_type and source_id",
            Self::EvidenceNotFound => "evidence must be a verbatim quote (5-2000 characters) of one reviewed field of the current text",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Written {
    Inserted,
    Updated,
    Unchanged,
}

/// Validate `review` against the programme's current `raw_payload` and
/// upsert it with a log row naming `actor`. With `require_text_hash` a
/// review without [`Review::text_hash`] is rejected. Runs in a savepoint of
/// the caller's transaction (or its own transaction).
pub async fn upsert(
    connection: &mut sqlx::PgConnection,
    review: &Review,
    actor: &str,
    require_text_hash: bool,
) -> Result<Result<Written, Rejection>, sqlx::Error> {
    use sqlx::Connection;
    let Some(class) = ApplicantClass::parse(review.class.trim()) else {
        return Ok(Err(Rejection::UnknownClass));
    };
    let reviewer = review.reviewer.trim();
    if !(1..=200).contains(&reviewer.chars().count()) || reviewer.chars().any(char::is_control) {
        return Ok(Err(Rejection::InvalidReviewer));
    }
    let source_type = review.source_type.trim();
    let source_id = review.source_id.trim();
    if !(1..=100).contains(&source_type.chars().count())
        || !(1..=200).contains(&source_id.chars().count())
    {
        return Ok(Err(Rejection::InvalidSource));
    }
    let read_hash = review
        .text_hash
        .as_deref()
        .map(|hash| hash.trim().to_ascii_lowercase());
    if require_text_hash && read_hash.is_none() {
        return Ok(Err(Rejection::MissingTextHash));
    }
    if fields(source_type).is_none() {
        return Ok(Err(Rejection::UnsupportedSource));
    }
    let mut tx = connection.begin().await?;
    let raw: Option<Option<Value>> = sqlx::query_scalar(
        "SELECT raw_payload FROM programs WHERE source_type = $1 AND source_id = $2",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(raw) = raw else {
        return Ok(Err(Rejection::ProgramNotFound));
    };
    let raw = raw.unwrap_or(Value::Null);
    let Some(hash) = text_hash(source_type, &raw) else {
        return Ok(Err(Rejection::UnsupportedSource));
    };
    if read_hash.is_some_and(|read| read != hash) {
        return Ok(Err(Rejection::TextChanged));
    }
    let evidence = review.evidence.trim();
    if !evidence_found(source_type, &raw, evidence) {
        return Ok(Err(Rejection::EvidenceNotFound));
    }
    let previous: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT class, evidence, reviewer, text_hash FROM program_applicant_reviews
         WHERE source_type = $1 AND source_id = $2 FOR UPDATE",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    let next = (
        class.as_str().to_owned(),
        evidence.to_owned(),
        reviewer.to_owned(),
        hash.clone(),
    );
    if previous.as_ref() == Some(&next) {
        tx.commit().await?;
        return Ok(Ok(Written::Unchanged));
    }
    sqlx::query(
        "INSERT INTO program_applicant_reviews (source_type, source_id, class, evidence, reviewer, text_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (source_type, source_id) DO UPDATE SET
           class = EXCLUDED.class, evidence = EXCLUDED.evidence, reviewer = EXCLUDED.reviewer,
           text_hash = EXCLUDED.text_hash, updated_at = now()",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(class.as_str())
    .bind(evidence)
    .bind(reviewer)
    .bind(&hash)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO program_applicant_review_log
           (source_type, source_id, action, class, previous_class, evidence, reviewer, text_hash, actor)
         VALUES ($1, $2, 'upsert', $3, $4, $5, $6, $7, $8)",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(class.as_str())
    .bind(previous.as_ref().map(|p| p.0.as_str()))
    .bind(evidence)
    .bind(reviewer)
    .bind(&hash)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(if previous.is_some() {
        Written::Updated
    } else {
        Written::Inserted
    }))
}

/// Delete a review with a log row. `false` = there was none.
pub async fn delete(
    connection: &mut sqlx::PgConnection,
    source_type: &str,
    source_id: &str,
    actor: &str,
) -> Result<bool, sqlx::Error> {
    use sqlx::Connection;
    let mut tx = connection.begin().await?;
    let previous: Option<String> = sqlx::query_scalar(
        "DELETE FROM program_applicant_reviews WHERE source_type = $1 AND source_id = $2
         RETURNING class",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    if previous.is_some() {
        sqlx::query(
            "INSERT INTO program_applicant_review_log
               (source_type, source_id, action, previous_class, actor)
             VALUES ($1, $2, 'delete', $3, $4)",
        )
        .bind(source_type)
        .bind(source_id)
        .bind(&previous)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(previous.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({
            "plcyNm": "청년일자리도약장려금",
            "plcyExplnCn": "중소기업이 청년을 정규직으로 채용하면 인건비를 지원",
            "plcySprtCn": "1인당 최대 1,200만원",
            "etcMttrCn": null,
            "addAplyQlfcCndCn": "참여기업: 5인 이상 우선지원대상기업",
            "plcyAplyMthdCn": "고용24에서 기업이 신청",
            "sbmsnDcmntCn": "",
            "srngMthdCn": "운영기관 심사",
            "zipCd": "11110"
        })
    }

    /// Golden value: changing the field list, order, separator or encoding
    /// invalidates every stored review, so it must be a deliberate change.
    /// Recompute with: python3 -c "import hashlib;print(hashlib.sha256('\x1f'.join(FIELDS).encode()).hexdigest())"
    #[test]
    fn text_hash_is_stable() {
        assert_eq!(
            text_hash("youth_center", &payload()).unwrap(),
            "8fcf35664ddb2160fd8504a7f58bf3e075eaaae3d126a3b0702d56a27e2a2e41"
        );
        assert_eq!(
            text_hash("youth_center", &json!({})).unwrap(),
            format!("{:x}", Sha256::digest("\u{1f}".repeat(8).as_bytes()))
        );
    }

    #[test]
    fn hash_reads_only_the_reviewed_fields() {
        let base = text_hash("youth_center", &payload());
        let mut other = payload();
        other["zipCd"] = json!("26410");
        other["lastMdfcnDt"] = json!("2026-10-15");
        assert_eq!(text_hash("youth_center", &other), base);
        other["addAplyQlfcCndCn"] = json!("만 19~34세 청년 누구나");
        assert_ne!(text_hash("youth_center", &other), base);
        // Moving text between fields is a different text.
        let mut moved = payload();
        moved["etcMttrCn"] = json!("1인당 최대 1,200만원");
        moved["plcySprtCn"] = json!(null);
        assert_ne!(text_hash("youth_center", &moved), base);
    }

    #[test]
    fn unsupported_sources_have_no_hash() {
        assert_eq!(text_hash("gov_benefits", &payload()), None);
        assert!(!evidence_found(
            "gov_benefits",
            &payload(),
            "고용24에서 기업이 신청"
        ));
    }

    #[test]
    fn evidence_must_be_a_quote_of_one_field() {
        let p = payload();
        assert!(evidence_found("youth_center", &p, "고용24에서 기업이 신청"));
        assert!(evidence_found("youth_center", &p, "  참여기업: 5인 이상 "));
        // Paraphrase, text from an unreviewed field, a span across two
        // fields, or a trivially short quote.
        assert!(!evidence_found(
            "youth_center",
            &p,
            "기업이 고용24에서 신청"
        ));
        assert!(!evidence_found("youth_center", &p, "11110"));
        assert!(!evidence_found(
            "youth_center",
            &p,
            "1,200만원\u{1f}참여기업"
        ));
        assert!(!evidence_found("youth_center", &p, "기업이"));
        assert!(!evidence_found("youth_center", &p, ""));
    }

    #[test]
    fn only_a_current_hash_hides_or_demotes() {
        use ApplicantClass::*;
        let h = "a".repeat(64);
        assert_eq!(effect(Organisation, &h, Some(&h)), ReviewEffect::Hide);
        assert_eq!(effect(Business, &h, Some(&h)), ReviewEffect::Hide);
        assert_eq!(effect(Indirect, &h, Some(&h)), ReviewEffect::Demote);
        for class in [Individual, InfoOnly, Unclear] {
            assert_eq!(effect(class, &h, Some(&h)), ReviewEffect::None);
        }
        let changed = "b".repeat(64);
        for class in ApplicantClass::ALL {
            assert_eq!(effect(class, &h, Some(&changed)), ReviewEffect::None);
            assert_eq!(effect(class, &h, None), ReviewEffect::None);
        }
    }

    #[test]
    fn class_names_match_the_table_check() {
        let names: Vec<_> = ApplicantClass::ALL.map(ApplicantClass::as_str).to_vec();
        assert_eq!(
            names,
            [
                "individual",
                "indirect",
                "organisation",
                "business",
                "info_only",
                "unclear"
            ]
        );
        for class in ApplicantClass::ALL {
            assert_eq!(ApplicantClass::parse(class.as_str()), Some(class));
            assert_eq!(serde_json::to_value(class).unwrap(), json!(class.as_str()));
        }
        assert_eq!(ApplicantClass::parse("Organisation"), None);
    }
}
