//! Passage index of one programme, built on the fly from the stored record:
//! the official text columns, whitelisted raw source fields, operator-approved
//! A-09 quotes and two record lines (registered window and amounts).
//!
//! Computed per request rather than stored: a programme's text is bounded
//! (each field is cut at [`MAX_FIELD_CHARS`]), splitting and scoring take
//! microseconds, and a stored index would need invalidation on every
//! collection run. Ids are `<field>.<n>` (`summary.0`, `raw.sprtTrgtCn.1`,
//! `record.window`), stable while the field text is unchanged.
use super::text;
use crate::assistant_provider::Topic;
use crate::services::evidence::text_label;
use chrono::{DateTime, Utc};
use mazimi_core::models::{CheckEvidence, EvidenceCheck, EvidenceKind, Program};
use serde::Serialize;
use serde_json::Value;

const MAX_FIELD_CHARS: usize = 2_000;
const MAX_PASSAGE_CHARS: usize = 300;
const LONG_SENTENCE_CHARS: usize = 160;
const CLAUSE_TARGET_CHARS: usize = 120;
const MAX_PASSAGES: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassageSection {
    Summary,
    Benefit,
    Conditions,
    Method,
    Documents,
    Screening,
    Period,
    Other,
}

impl PassageSection {
    /// The assistant topic a passage of this section answers.
    pub fn topic(self) -> Option<Topic> {
        match self {
            Self::Summary | Self::Benefit => Some(Topic::Summary),
            Self::Conditions => Some(Topic::Eligibility),
            Self::Method | Self::Screening => Some(Topic::Steps),
            Self::Documents => Some(Topic::Documents),
            Self::Period => Some(Topic::Deadline),
            Self::Other => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassageKind {
    /// A sentence of the stored notice text, verbatim (contacts masked).
    OfficialText,
    /// An operator-approved A-09 quote not already contained in the text.
    ApprovedQuote,
    /// A registered column (window, amount) formatted as one line.
    Record,
}

/// Char offsets (Unicode scalar values, not UTF-16 units) into `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Highlight {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Passage {
    pub id: String,
    pub text: String,
    pub highlights: Vec<Highlight>,
    pub section: PassageSection,
    pub kind: PassageKind,
    pub source_label: String,
    pub captured_at: Option<DateTime<Utc>>,
    /// Set when an operator approved a quote inside this passage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
}

/// Raw upstream text fields worth quoting, in index order. Other raw keys
/// (codes, URLs, contacts, counters, unknown new fields) are not quoted:
/// an explicit whitelist keeps provenance-only payload out of answers.
const RAW_FIELDS: &[(&str, PassageSection, &str)] = &[
    // 온통청년
    ("plcyExplnCn", PassageSection::Summary, "정책 설명"),
    ("plcySprtCn", PassageSection::Benefit, "지원 내용"),
    ("sprtTrgtCn", PassageSection::Conditions, "지원 대상"),
    (
        "addAplyQlfcCndCn",
        PassageSection::Conditions,
        "추가 신청 자격",
    ),
    (
        "ptcpPrpTrgtCn",
        PassageSection::Conditions,
        "참여 제한 대상",
    ),
    ("earnEtcCn", PassageSection::Conditions, "소득 조건"),
    ("plcyAplyMthdCn", PassageSection::Method, "신청방법"),
    ("sbmsnDcmntCn", PassageSection::Documents, "제출서류"),
    ("srngMthdCn", PassageSection::Screening, "심사방법"),
    ("aplyYmd", PassageSection::Period, "신청 기간"),
    ("etcMttrCn", PassageSection::Other, "기타 사항"),
    // 정부24
    ("서비스목적요약", PassageSection::Summary, "서비스 목적"),
    ("지원대상", PassageSection::Conditions, "지원 대상"),
    ("선정기준", PassageSection::Conditions, "선정 기준"),
    ("지원내용", PassageSection::Benefit, "지원 내용"),
    ("지원유형", PassageSection::Benefit, "지원 유형"),
    ("신청방법", PassageSection::Method, "신청방법"),
    ("신청기한", PassageSection::Period, "신청 기한"),
    ("구비서류", PassageSection::Documents, "제출서류"),
    // 한국사회보장정보원 복지서비스
    ("servDgst", PassageSection::Summary, "서비스 요약"),
    ("sprtCycNm", PassageSection::Benefit, "지원 주기"),
    ("srvPvsnNm", PassageSection::Benefit, "제공 유형"),
    ("aplyMtdNm", PassageSection::Method, "신청방법"),
];

fn bounded(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_PASSAGE_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_PASSAGE_CHARS).collect();
    cut.push('…');
    cut
}

fn is_bullet(c: char) -> bool {
    matches!(
        c,
        '○' | '●' | '◦' | '•' | '▪' | '■' | '□' | '◇' | '◆' | '※' | '▶' | '►'
    ) || ('\u{2460}'..='\u{2473}').contains(&c)
}

fn content_chars(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}

/// Split long sentences at `, ` / `; ` into clauses of about
/// [`CLAUSE_TARGET_CHARS`].
fn clauses(sentence: &str) -> Vec<String> {
    if sentence.chars().count() <= LONG_SENTENCE_CHARS {
        return vec![sentence.to_owned()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut rest = sentence;
    while let Some(at) = rest.find([',', ';']) {
        let (head, tail) = rest.split_at(at + 1);
        current.push_str(head);
        rest = tail;
        if current.chars().count() >= CLAUSE_TARGET_CHARS {
            out.push(current.trim().to_owned());
            current.clear();
        }
    }
    current.push_str(rest);
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// Sentences of a field: one per line, then at sentence enders (`.`, `!`,
/// `?` followed by a space or the end; `2026.10.01` stays whole) and before
/// bullet marks. Long sentences become clauses.
pub(crate) fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let chars: Vec<char> = line.chars().collect();
        let mut current = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if is_bullet(c) && !current.trim().is_empty() {
                sentences.push(std::mem::take(&mut current));
            }
            if is_bullet(c) {
                continue;
            }
            current.push(c);
            if super::text::ends_sentence(&chars, i) {
                sentences.push(std::mem::take(&mut current));
            }
        }
        if !current.trim().is_empty() {
            sentences.push(current);
        }
    }
    sentences
        .iter()
        .map(|s| s.trim())
        .flat_map(clauses)
        .filter(|s| content_chars(s) >= 2)
        .collect()
}

struct Builder<'a> {
    program: &'a Program,
    passages: Vec<Passage>,
    seen_fields: Vec<String>,
}

impl Builder<'_> {
    fn field(&mut self, key: &str, section: PassageSection, label: &str, value: Option<&str>) {
        let Some(value) = value
            .map(|v| mazimi_worker::enrichment::redact_bounded(v, MAX_FIELD_CHARS))
            .filter(|v| !v.trim().is_empty())
        else {
            return;
        };
        let normalized = text::compact(&value).0;
        if self.seen_fields.contains(&normalized) {
            return;
        }
        self.seen_fields.push(normalized);
        let source_label = text_label(self.program, Some(label));
        for (n, sentence) in split_sentences(&value).into_iter().enumerate() {
            self.push(Passage {
                id: format!("{key}.{n}"),
                text: bounded(&sentence),
                highlights: Vec::new(),
                section,
                kind: PassageKind::OfficialText,
                source_label: source_label.clone(),
                captured_at: self.program.last_synced_at,
                approved_at: None,
            });
        }
    }

    fn push(&mut self, passage: Passage) {
        if self.passages.len() >= MAX_PASSAGES {
            return;
        }
        let key = text::compact(&passage.text).0;
        if self
            .passages
            .iter()
            .any(|p| text::compact(&p.text).0 == key)
        {
            return;
        }
        self.passages.push(passage);
    }
}

fn check_key(check: EvidenceCheck) -> (&'static str, PassageSection) {
    match check {
        EvidenceCheck::Age => ("age", PassageSection::Conditions),
        EvidenceCheck::Region => ("region", PassageSection::Conditions),
        EvidenceCheck::Enrollment => ("enrollment", PassageSection::Conditions),
        EvidenceCheck::HardRule => ("hard_rule", PassageSection::Conditions),
        EvidenceCheck::ApplicationWindow => ("application_window", PassageSection::Period),
        EvidenceCheck::Documents => ("documents", PassageSection::Documents),
    }
}

fn kst(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).expect("KST offset"))
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn won(amount: i32) -> String {
    let digits = amount.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("{}{out}원", if amount < 0 { "-" } else { "" })
}

fn records(program: &Program) -> Vec<Passage> {
    let record = |id: &str, section, label: &str, text: String| Passage {
        id: id.to_owned(),
        text,
        highlights: Vec::new(),
        section,
        kind: PassageKind::Record,
        source_label: format!("마지미 등록 정보 · {label}"),
        captured_at: Some(program.updated_at),
        approved_at: None,
    };
    let mut out = Vec::new();
    let window = match (program.application_start_at, program.effective_deadline()) {
        (Some(start), Some(end)) => Some(format!(
            "등록된 신청 기간: {} ~ {} (한국 시간)",
            kst(start),
            kst(end)
        )),
        (None, Some(end)) => Some(format!("등록된 신청 마감: {} (한국 시간)", kst(end))),
        (Some(start), None) => Some(format!("등록된 신청 시작: {} (한국 시간)", kst(start))),
        (None, None) => None,
    };
    if let Some(text) = window {
        out.push(record(
            "record.window",
            PassageSection::Period,
            "신청 기간",
            text,
        ));
    }
    // Each amount keeps its own period; they are never added up. A corporate
    // offer's amounts are no support money (pays_support).
    let paid = mazimi_core::pays_support(&program.program_type);
    let amounts: Vec<String> = [
        program
            .benefit_amount_monthly
            .map(|n| format!("월 {}", won(n))),
        program
            .benefit_amount_semester
            .map(|n| format!("학기당 {}", won(n))),
        program
            .benefit_amount_once
            .map(|n| format!("1회 {}", won(n))),
    ]
    .into_iter()
    .flatten()
    .filter(|_| paid)
    .collect();
    if !amounts.is_empty() {
        out.push(record(
            "record.amount",
            PassageSection::Benefit,
            "지원 금액",
            format!("등록된 지원 금액: {}", amounts.join(" · ")),
        ));
    }
    out
}

/// Every passage of a programme, in index order. `evidence` is the
/// programme's citation set from `services::evidence::load`.
pub fn build(program: &Program, evidence: &[CheckEvidence]) -> Vec<Passage> {
    let mut builder = Builder {
        program,
        passages: Vec::new(),
        seen_fields: Vec::new(),
    };
    builder.field(
        "summary",
        PassageSection::Summary,
        "요약",
        program.summary.as_deref(),
    );
    let conditions = program
        .normalized_payload
        .as_ref()
        .and_then(|p| p.get("conditions"))
        .and_then(Value::as_str);
    builder.field(
        "conditions",
        PassageSection::Conditions,
        "지원 조건",
        conditions,
    );
    builder.field(
        "method",
        PassageSection::Method,
        "신청방법",
        program.application_method.as_deref(),
    );
    builder.field(
        "documents",
        PassageSection::Documents,
        "제출서류",
        program.submission_documents.as_deref(),
    );
    builder.field(
        "screening",
        PassageSection::Screening,
        "심사방법",
        program.screening_method.as_deref(),
    );
    if let Some(raw) = program.raw_payload.as_ref().filter(|r| r.is_object()) {
        for (key, section, label) in RAW_FIELDS {
            let value = raw[*key].as_str().map(str::trim).filter(|v| {
                !v.starts_with("http://") && !v.starts_with("https://") && content_chars(v) >= 2
            });
            builder.field(&format!("raw.{key}"), *section, label, value);
        }
    }
    let mut approved_counts = std::collections::HashMap::new();
    for cited in evidence {
        let item = &cited.evidence;
        let (EvidenceKind::AiExtractedApproved, Some(quote)) = (item.kind, item.quote.as_deref())
        else {
            continue;
        };
        let needle = text::compact(quote.trim_end_matches('…')).0;
        if needle.is_empty() {
            continue;
        }
        if let Some(existing) = builder
            .passages
            .iter_mut()
            .find(|p| text::compact(&p.text).0.contains(&needle))
        {
            existing.approved_at = existing.approved_at.or(item.approved_at);
            continue;
        }
        let (key, section) = check_key(cited.check);
        let n = approved_counts.entry(key).or_insert(0usize);
        builder.push(Passage {
            id: format!("approved.{key}.{n}"),
            text: bounded(quote),
            highlights: Vec::new(),
            section,
            kind: PassageKind::ApprovedQuote,
            source_label: item.source_label.clone(),
            captured_at: item.captured_at,
            approved_at: item.approved_at,
        });
        *n += 1;
    }
    for record in records(program) {
        builder.push(record);
    }
    builder.passages
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mazimi_core::models::Evidence;
    use serde_json::json;

    pub(crate) fn program(fields: Value) -> Program {
        let mut base = json!({
            "id": uuid::Uuid::nil(), "program_type": "policy", "source_type": "youth_center",
            "source_id": "R1", "title": "부산 청년 월세 지원", "summary": null, "provider_name": null,
            "official_url": "https://www.youthcenter.go.kr/x", "program_status": "active",
            "application_start_at": null, "application_end_at": null,
            "benefit_amount_monthly": null, "benefit_amount_semester": null,
            "benefit_amount_once": null, "region_scope": null, "school_scope": null,
            "tags": null, "raw_payload": null, "normalized_payload": null, "min_age": null, "max_age": null,
            "regions": null, "deadline_at": null, "benefit_valid_until": null, "is_active": true,
            "last_synced_at": "2026-10-01T00:00:00Z", "created_at": "2026-09-01T00:00:00Z",
            "updated_at": "2026-10-02T00:00:00Z", "company_name": null, "company_logo_url": null,
            "benefit_category": null, "provider_kind": null, "application_steps": null,
            "application_method": null, "submission_documents": null, "screening_method": null
        });
        for (key, value) in fields.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn sentences_split_at_enders_bullets_and_lines_but_not_dates() {
        let text = "부산 거주 청년. 신청기간: 2026.10.01 ~ 2027.12.31\n○ 무주택자 ○ 소득 8분위 이하\n- 신분증";
        assert_eq!(
            split_sentences(text),
            [
                "부산 거주 청년.",
                "신청기간: 2026.10.01 ~ 2027.12.31",
                "무주택자",
                "소득 8분위 이하",
                "신분증"
            ]
        );
        let long = format!(
            "{}, {}, {}",
            "가".repeat(100),
            "나".repeat(100),
            "다".repeat(30)
        );
        let parts = split_sentences(&long);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts.iter().all(|p| p.chars().count() <= 240));
    }

    #[test]
    fn a_corporate_offer_has_no_support_amount_passage() {
        let p = program(json!({
            "program_type": "corporate_benefit",
            "summary": "대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다.",
            "benefit_amount_monthly": 5000
        }));
        assert!(build(&p, &[]).iter().all(|p| p.id != "record.amount"));
    }

    #[test]
    fn passages_have_stable_ids_labels_and_masked_contacts() {
        let p = program(json!({
            "summary": "부산 거주 만 19~34세 청년에게 월 최대 20만원 월세를 지원합니다. 문의 010-1234-5678",
            "application_method": "온라인 신청",
            "submission_documents": "신분증, 임대차계약서",
            "raw_payload": {"sprtTrgtCn": "무주택 청년", "plcyAplyMthdCn": "온라인 신청",
                            "aplyUrlAddr": "https://young.busan.go.kr", "zipCd": "26110"},
            "application_end_at": "2026-12-31T14:59:59Z",
            "benefit_amount_monthly": 200000
        }));
        let passages = build(&p, &[]);
        let ids: Vec<_> = passages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "summary.0",
                "summary.1",
                "method.0",
                "documents.0",
                "raw.sprtTrgtCn.0",
                "record.window",
                "record.amount"
            ]
        );
        assert!(
            !passages[1].text.contains("1234-5678"),
            "{}",
            passages[1].text
        );
        assert_eq!(passages[3].source_label, "온통청년 공고 원문 · 제출서류");
        assert_eq!(
            passages[5].text,
            "등록된 신청 마감: 2026-12-31 23:59 (한국 시간)"
        );
        assert_eq!(passages[6].text, "등록된 지원 금액: 월 200,000원");
        assert_eq!(passages[6].kind, PassageKind::Record);
    }

    #[test]
    fn approved_quotes_mark_their_sentence_or_become_passages() {
        let p = program(json!({"summary": "만 19세~34세 청년 대상."}));
        let approved = |quote: &str, check| CheckEvidence {
            check,
            evidence: Evidence {
                kind: EvidenceKind::AiExtractedApproved,
                quote: Some(quote.into()),
                source_label: "온통청년 공고 원문 · 요약".into(),
                source_url: None,
                captured_at: None,
                approved_at: Some(Utc::now()),
                subject: None,
            },
        };
        let passages = build(
            &p,
            &[
                approved("만 19세~34세", EvidenceCheck::Age),
                approved("소득분위 8분위 이하", EvidenceCheck::HardRule),
            ],
        );
        assert!(passages[0].approved_at.is_some());
        assert_eq!(passages[1].id, "approved.hard_rule.0");
        assert_eq!(passages[1].kind, PassageKind::ApprovedQuote);
        assert_eq!(passages[1].section, PassageSection::Conditions);
    }
}
