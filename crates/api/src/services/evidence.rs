//! Citations behind eligibility, document and deadline explanations
//! (product plan §4.2: every fact has a source). Deterministic from stored
//! data; nothing here calls a model or paraphrases.
//!
//! Three kinds, each cited only while it still supports the stored value:
//! - `ai_extracted_approved`: an operator-approved A-09 fact. Its quote is a
//!   span of the notice text the reviewer checked. Shown only while the
//!   program's current source text hashes to the one the span was verified
//!   against (a changed notice supersedes it) and the approved value is still
//!   the public one (age columns, window columns, a hard-filter condition, a
//!   listed document).
//! - `structured_source`: a structured upstream field (온통청년 연령·지역
//!   코드·신청기간) the stored column still equals. There is no sentence, so
//!   there is no quote.
//! - `official_text`: the notice's own 제출서류 text, while the stored column
//!   still equals the upstream field.
use super::ai_extraction::{rule_condition, same_condition};
use super::benefit_assistant::safe_official_url;
use chrono::{DateTime, NaiveDate, Utc};
use mazimi_core::models::{CheckEvidence, Evidence, EvidenceCheck, EvidenceKind, Program};
use mazimi_worker::ai_extraction::{
    facts::{document_key, Evidence as Span, FactValue, WindowKind},
    job::PROGRAM_TEXT_COLUMNS,
    source::{build_source, ProgramText},
};
use mazimi_worker::pipeline::{kst_end_of_day, kst_start_of_day};
use serde_json::Value;
use sqlx::PgConnection;
use std::collections::HashMap;
use uuid::Uuid;

/// Longest quote shown; longer official text is cut at a char boundary
/// and marked with an ellipsis, never rewritten.
const MAX_QUOTE_CHARS: usize = 300;

/// `raw_payload` projection for list queries: only the structured fields a
/// citation compares, instead of the whole provenance payload per row.
pub(crate) const LIST_RAW_PAYLOAD_SQL: &str = "CASE WHEN source_type = 'youth_center' THEN \
     jsonb_build_object('sprtTrgtMinAge', raw_payload->'sprtTrgtMinAge', \
     'sprtTrgtMaxAge', raw_payload->'sprtTrgtMaxAge', 'zipCd', raw_payload->'zipCd', \
     'aplyYmd', raw_payload->'aplyYmd') END";

/// Public name of a collecting source. `None` for operator-entered or
/// unrecognised rows, whose text is not claimed to be a notice's original.
fn source_name(source_type: &str) -> Option<&'static str> {
    Some(match source_type {
        "youth_center" => "온통청년",
        "gov_benefits" => "정부24",
        "scholarship" => "한국장학재단",
        "national_welfare" => "한국사회보장정보원 중앙부처 복지서비스",
        "local_welfare" => "한국사회보장정보원 지자체 복지서비스",
        "fss_financial" => "금융감독원 금융상품 비교공시",
        _ => return None,
    })
}

pub(crate) fn text_label(program: &Program, section: Option<&str>) -> String {
    let base = source_name(&program.source_type)
        .map(|name| format!("{name} 공고 원문"))
        .unwrap_or_else(|| "마지미에 등록된 공고 내용".to_owned());
    match section {
        Some(section) => format!("{base} · {section}"),
        None => base,
    }
}

fn bounded_quote(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_QUOTE_CHARS {
        return Some(text.to_owned());
    }
    let mut cut: String = text.chars().take(MAX_QUOTE_CHARS).collect();
    cut.push('…');
    Some(cut)
}

fn cite(check: EvidenceCheck, evidence: Evidence) -> CheckEvidence {
    CheckEvidence { check, evidence }
}

fn structured(program: &Program, field: &str) -> Evidence {
    Evidence {
        kind: EvidenceKind::StructuredSource,
        quote: None,
        source_label: format!(
            "{} 공고 데이터 · {field}",
            source_name(&program.source_type).unwrap_or("수집처")
        ),
        source_url: safe_official_url(program.official_url.as_deref()),
        captured_at: program.last_synced_at,
        approved_at: None,
        subject: None,
    }
}

fn youth_age(raw: &Value, key: &str) -> Option<i32> {
    raw[key]
        .as_str()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|age| (1..=100).contains(age))
}

fn youth_window(raw: &Value) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let Some(value) = raw["aplyYmd"].as_str() else {
        return (None, None);
    };
    let mut parts = value
        .split('~')
        .map(|part| NaiveDate::parse_from_str(part.trim(), "%Y%m%d").ok());
    (
        parts.next().flatten().map(kst_start_of_day),
        parts.next().flatten().map(kst_end_of_day),
    )
}

/// Citations derivable from the programme row alone.
pub(crate) fn from_record(program: &Program) -> Vec<CheckEvidence> {
    let mut out = Vec::new();
    let Some(raw) = program.raw_payload.as_ref().filter(|r| r.is_object()) else {
        return out;
    };
    if program.source_type == "youth_center" {
        if (program.min_age.is_some() || program.max_age.is_some())
            && youth_age(raw, "sprtTrgtMinAge") == program.min_age
            && youth_age(raw, "sprtTrgtMaxAge") == program.max_age
        {
            out.push(cite(EvidenceCheck::Age, structured(program, "지원 연령")));
        }
        if let Some(stored) = program
            .regions
            .as_ref()
            .filter(|r| !r.is_empty() && !r.iter().any(|r| r == "unknown"))
        {
            let mut stored = stored.clone();
            stored.sort_unstable();
            if mazimi_worker::regions::youth(raw) == stored {
                out.push(cite(
                    EvidenceCheck::Region,
                    structured(program, "지역 코드"),
                ));
            }
        }
        let (start, end) = youth_window(raw);
        if (start.is_some() || end.is_some())
            && start == program.application_start_at
            && end == program.application_end_at
        {
            out.push(cite(
                EvidenceCheck::ApplicationWindow,
                structured(program, "신청 기간"),
            ));
        }
    }
    // The source field the stored documents text was taken from (보조금24:
    // the serviceDetail row under the detail key).
    let upstream = match program.source_type.as_str() {
        "youth_center" => raw["sbmsnDcmntCn"].as_str().map(|u| u.trim().to_owned()),
        "gov_benefits" => mazimi_worker::sources::gov_benefits::detail_documents(raw),
        _ => None,
    };
    if let (Some(upstream), Some(stored)) = (upstream, program.submission_documents.as_deref()) {
        if !upstream.is_empty() && upstream == stored.trim() {
            // Same contact masking the evidence source text gets.
            let masked = mazimi_worker::enrichment::redact_bounded(stored, usize::MAX);
            if let Some(quote) = bounded_quote(&masked) {
                out.push(cite(
                    EvidenceCheck::Documents,
                    Evidence {
                        kind: EvidenceKind::OfficialText,
                        quote: Some(quote),
                        source_label: text_label(program, Some("제출서류")),
                        source_url: safe_official_url(program.official_url.as_deref()),
                        captured_at: program.last_synced_at,
                        approved_at: None,
                        subject: None,
                    },
                ));
            }
        }
    }
    out
}

/// Section of the assembled source text a char offset falls in, when it is
/// one of the named columns (`[요약] …`). Raw upstream keys stay unnamed.
fn section_at(text: &str, start: usize) -> Option<&'static str> {
    let prefix: String = text.chars().take(start).collect();
    let header = prefix
        .rmatch_indices('[')
        .find(|(i, _)| *i == 0 || prefix[..*i].ends_with('\n'))
        .map(|(i, _)| &prefix[i + 1..])?;
    let label = header.split_once("] ")?.0;
    ["제목", "요약", "신청방법", "제출서류", "심사방법"]
        .into_iter()
        .find(|known| *known == label)
}

/// Whether `condition` is a required condition of the tree: the root leaf
/// or a member of nested `all` groups. A match inside `any` is one way to
/// qualify, not a required condition, and is not cited as one.
fn tree_contains(node: &Value, condition: &Value) -> bool {
    if let Some(children) = node.get("all").and_then(Value::as_array) {
        return children.iter().any(|child| tree_contains(child, condition));
    }
    node.get("any").is_none() && same_condition(node, condition)
}

#[derive(sqlx::FromRow)]
struct ApprovedRow {
    program_id: Uuid,
    fact_key: String,
    source_hash: String,
    value: Value,
    evidence: Option<Value>,
    reviewed_at: Option<DateTime<Utc>>,
    checked_at: DateTime<Utc>,
    document_names: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct SourceRow {
    id: Uuid,
    #[sqlx(flatten)]
    text: ProgramText,
}

/// The check an approved fact still supports, and the document name it is
/// about, or `None` when the public value moved away from it.
fn approved_check(
    program: &Program,
    hard_filter: Option<&Value>,
    value: &FactValue,
    documents: &[String],
) -> Option<(EvidenceCheck, Option<String>)> {
    match value {
        FactValue::Age {
            min_age, max_age, ..
        } => ((min_age.is_some() || max_age.is_some())
            && min_age.is_none_or(|v| program.min_age == Some(v))
            && max_age.is_none_or(|v| program.max_age == Some(v)))
        .then_some((EvidenceCheck::Age, None)),
        FactValue::ApplicationWindow {
            kind: WindowKind::Dated,
            start_date,
            end_date,
        } => ((start_date.is_some() || end_date.is_some())
            && start_date
                .is_none_or(|d| program.application_start_at == Some(kst_start_of_day(d)))
            && end_date.is_none_or(|d| program.application_end_at == Some(kst_end_of_day(d))))
        .then_some((EvidenceCheck::ApplicationWindow, None)),
        // 상시·예산 소진 are recorded only; the approved sentence describes
        // the window only while no dated window has been set since.
        FactValue::ApplicationWindow { .. } => (program.application_start_at.is_none()
            && program.application_end_at.is_none())
        .then_some((EvidenceCheck::ApplicationWindow, None)),
        FactValue::Document { name, .. } => {
            let key = document_key(name);
            documents
                .iter()
                .find(|listed| document_key(listed) == key)
                .map(|listed| (EvidenceCheck::Documents, Some(listed.clone())))
        }
        other => {
            let condition = rule_condition(other)?;
            tree_contains(hard_filter?, &condition).then_some((EvidenceCheck::HardRule, None))
        }
    }
}

/// Every citation for each programme: [`from_record`] plus approved AI
/// facts still grounded in the current source. Each programme comes with the
/// hard filter of its latest rule version. Two indexed queries; the source
/// text is rebuilt only for programmes that have approved facts.
pub(crate) async fn load(
    connection: &mut PgConnection,
    programs: &[(&Program, Option<&Value>)],
) -> Result<HashMap<Uuid, Vec<CheckEvidence>>, sqlx::Error> {
    let mut out: HashMap<Uuid, Vec<CheckEvidence>> = programs
        .iter()
        .map(|(program, _)| (program.id, from_record(program)))
        .collect();
    let ids: Vec<Uuid> = programs.iter().map(|(p, _)| p.id).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let approved = sqlx::query_as::<_, ApprovedRow>(
        "SELECT c.program_id, c.fact_key, c.source_hash, c.value, c.evidence, c.reviewed_at, s.checked_at,
                COALESCE((SELECT array_agg(d.document_name ORDER BY d.sort_order, d.document_name)
                          FROM program_documents d WHERE d.program_id = c.program_id),
                         '{}') AS document_names
         FROM program_ai_extraction_candidates c
         JOIN program_ai_extraction_cache s ON s.id = c.cache_id
         WHERE c.program_id = ANY($1) AND c.status = 'approved'
         ORDER BY c.program_id, c.fact_type, c.fact_key, c.reviewed_at DESC NULLS LAST, c.id",
    )
    .bind(&ids)
    .fetch_all(&mut *connection)
    .await?;
    if approved.is_empty() {
        return Ok(out);
    }
    let mut with_facts: Vec<Uuid> = approved.iter().map(|r| r.program_id).collect();
    with_facts.dedup();
    let sources: HashMap<Uuid, (String, String)> = sqlx::query_as::<_, SourceRow>(&format!(
        "SELECT p.id, {PROGRAM_TEXT_COLUMNS} FROM programs p WHERE p.id = ANY($1)"
    ))
    .bind(&with_facts)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|row| {
        let source = build_source(&row.text);
        (row.id, (source.hash, source.text))
    })
    .collect();
    let by_id: HashMap<Uuid, (&Program, Option<&Value>)> =
        programs.iter().map(|(p, r)| (p.id, (*p, *r))).collect();
    // Several approved rows can share a fact (another model or prompt
    // version over the same text): cite only the newest still-valid one.
    let mut cited: std::collections::HashSet<(Uuid, String)> = std::collections::HashSet::new();
    let mut stale = 0usize;
    for row in approved {
        if cited.contains(&(row.program_id, row.fact_key.clone())) {
            continue;
        }
        let (Some((hash, text)), Some((program, rule))) =
            (sources.get(&row.program_id), by_id.get(&row.program_id))
        else {
            continue;
        };
        if *hash != row.source_hash {
            stale += 1;
            continue;
        }
        let (Ok(value), Some(Ok(span))) = (
            serde_json::from_value::<FactValue>(row.value),
            row.evidence.map(serde_json::from_value::<Span>),
        ) else {
            continue;
        };
        // Same hash means same text; re-check the span anyway so a quote is
        // never shown that the current text does not contain verbatim.
        let current: String = text
            .chars()
            .skip(span.start)
            .take(span.end.saturating_sub(span.start))
            .collect();
        if current != span.quote || span.quote.trim().is_empty() {
            continue;
        }
        let Some((check, subject)) = approved_check(program, *rule, &value, &row.document_names)
        else {
            continue;
        };
        cited.insert((row.program_id, row.fact_key.clone()));
        out.entry(row.program_id).or_default().push(cite(
            check,
            Evidence {
                kind: EvidenceKind::AiExtractedApproved,
                quote: bounded_quote(&span.quote),
                source_label: text_label(program, section_at(text, span.start)),
                source_url: safe_official_url(program.official_url.as_deref()),
                captured_at: Some(row.checked_at),
                approved_at: row.reviewed_at,
                subject,
            },
        ));
    }
    if stale > 0 {
        // Fails closed (no quote), but a source-assembly change or an
        // API/worker version skew would drop every citation silently.
        tracing::debug!(stale, "approved facts not cited: source text changed");
    }
    Ok(out)
}

/// Citations for one check, without the tag.
pub(crate) fn for_check(evidence: &[CheckEvidence], check: EvidenceCheck) -> Vec<Evidence> {
    evidence
        .iter()
        .filter(|e| e.check == check)
        .map(|e| e.evidence.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn youth(raw: Value) -> Program {
        let mut p: Program = serde_json::from_value(json!({
            "id": Uuid::nil(), "program_type": "policy", "source_type": "youth_center",
            "source_id": "R1", "title": "t", "summary": null, "provider_name": null,
            "official_url": "https://www.youthcenter.go.kr/x", "program_status": "active",
            "application_start_at": null, "application_end_at": null,
            "benefit_amount_monthly": null, "benefit_amount_semester": null,
            "benefit_amount_once": null, "region_scope": null, "school_scope": null,
            "tags": null, "normalized_payload": null, "min_age": 19, "max_age": 34,
            "regions": ["busan"], "deadline_at": null, "is_active": true,
            "last_synced_at": null, "created_at": Utc::now(), "updated_at": Utc::now(),
            "company_name": null, "company_logo_url": null, "benefit_category": null,
            "provider_kind": null, "application_steps": null, "application_method": null,
            "submission_documents": "신분증", "screening_method": null
        }))
        .unwrap();
        p.raw_payload = Some(raw);
        p
    }

    fn checks(evidence: &[CheckEvidence]) -> Vec<EvidenceCheck> {
        evidence.iter().map(|e| e.check).collect()
    }

    #[test]
    fn structured_fields_are_cited_only_while_the_column_matches() {
        let raw = json!({"sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "34", "zipCd": "26110,26140",
                         "aplyYmd": "20261001 ~ 20271231", "sbmsnDcmntCn": " 신분증 "});
        let mut p = youth(raw.clone());
        p.application_start_at = Some(kst_start_of_day(
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        ));
        p.application_end_at = Some(kst_end_of_day(
            NaiveDate::from_ymd_opt(2027, 12, 31).unwrap(),
        ));
        let all = from_record(&p);
        assert_eq!(
            checks(&all),
            [
                EvidenceCheck::Age,
                EvidenceCheck::Region,
                EvidenceCheck::ApplicationWindow,
                EvidenceCheck::Documents
            ]
        );
        assert!(all[..3].iter().all(|e| e.evidence.quote.is_none()));
        assert_eq!(all[3].evidence.quote.as_deref(), Some("신분증"));
        assert_eq!(all[3].evidence.kind, EvidenceKind::OfficialText);
        assert_eq!(
            all[0].evidence.source_label,
            "온통청년 공고 데이터 · 지원 연령"
        );

        p.max_age = Some(39);
        p.regions = Some(vec!["unknown".into()]);
        p.submission_documents = Some("신분증, 통장 사본".into());
        p.application_end_at = None;
        assert!(
            from_record(&p).is_empty(),
            "edited values are not attributed"
        );

        let mut other = youth(raw);
        other.source_type = "manual".into();
        assert!(from_record(&other).is_empty());
    }

    #[test]
    fn long_official_text_is_cut_not_rewritten() {
        let text = "가".repeat(MAX_QUOTE_CHARS + 5);
        let quote = bounded_quote(&text).unwrap();
        assert_eq!(quote.chars().count(), MAX_QUOTE_CHARS + 1);
        assert!(quote.ends_with('…') && text.starts_with(quote.trim_end_matches('…')));
        assert_eq!(bounded_quote("  "), None);
    }

    #[test]
    fn section_labels_come_from_named_headers_only() {
        let text = "[제목] 월세 지원\n[요약] 만 19세~34세 청년\n줄바꿈 [가짜] 문장\n[sprtTrgtCn] 소득 8분위";
        let at = |needle: &str| {
            let byte = text.find(needle).unwrap();
            section_at(text, text[..byte].chars().count())
        };
        assert_eq!(at("만 19세"), Some("요약"));
        assert_eq!(
            at("[가짜] 문장"),
            Some("요약"),
            "a bracket mid-line is not a header"
        );
        assert_eq!(at("소득 8분위"), None, "raw upstream keys stay unnamed");
        assert_eq!(at("월세"), Some("제목"));
    }

    #[test]
    fn approved_rule_facts_need_the_condition_in_the_current_hard_filter() {
        let p = youth(json!({}));
        let income: FactValue = serde_json::from_value(json!({
            "type": "income", "criterion": "income_decile", "comparator": "lte", "value": 8
        }))
        .unwrap();
        let kept = json!({"all": [{"all": [
            {"field": "income_bracket", "op": "lte", "value": 8, "weight": 1.0}
        ]}]});
        let alternative = json!({"all": [{"any": [
            {"field": "income_bracket", "op": "lte", "value": 8, "weight": 1.0},
            {"field": "kosaf_support_bracket", "op": "lte", "value": 3, "weight": 1.0}
        ]}]});
        assert_eq!(
            approved_check(&p, Some(&alternative), &income, &[]),
            None,
            "one alternative of an any-group is not a required condition"
        );
        assert_eq!(
            approved_check(&p, Some(&kept), &income, &[]),
            Some((EvidenceCheck::HardRule, None))
        );
        let edited = json!({"field": "income_bracket", "op": "lte", "value": 6});
        assert_eq!(approved_check(&p, Some(&edited), &income, &[]), None);
        assert_eq!(approved_check(&p, None, &income, &[]), None);

        let document: FactValue = serde_json::from_value(json!({
            "type": "document", "name": "주민등록등본", "required": true, "notes": null
        }))
        .unwrap();
        assert_eq!(
            approved_check(&p, None, &document, &["주민등록 등본".into()]),
            Some((EvidenceCheck::Documents, Some("주민등록 등본".into())))
        );
        assert_eq!(approved_check(&p, None, &document, &[]), None);

        let age: FactValue = serde_json::from_value(json!({
            "type": "age", "min_age": 19, "max_age": 39, "basis": "man_age"
        }))
        .unwrap();
        assert_eq!(
            approved_check(&p, None, &age, &[]),
            None,
            "value since changed"
        );

        let always: FactValue = serde_json::from_value(json!({
            "type": "application_window", "kind": "always_open",
            "start_date": null, "end_date": null
        }))
        .unwrap();
        assert_eq!(
            approved_check(&p, None, &always, &[]),
            Some((EvidenceCheck::ApplicationWindow, None))
        );
        let mut dated = youth(json!({}));
        dated.application_end_at = Some(Utc::now());
        assert_eq!(
            approved_check(&dated, None, &always, &[]),
            None,
            "a 상시 quote never sits next to a dated window"
        );
    }
}
