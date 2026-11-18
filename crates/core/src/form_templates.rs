//! 서류 작성 도우미 field definitions for a verified official PDF template.
//!
//! An admin draws input boxes once per template version; the mobile app then
//! lets users fill those boxes on their own device. This module validates the
//! definition only. It never carries user answers. The mobile mirror is
//! `apps/mobile/lib/formFill/fields.ts::parseTemplate`; keep both in step.
use serde::{Deserialize, Serialize};

pub const MAX_FIELDS: usize = 200;
pub const MAX_PAGES: i32 = 50;
pub const FIELD_TYPES: &[&str] = &[
    "text",
    "multiline",
    "date",
    "checkbox",
    "signature",
    "stamp",
];
/// Profile/memory keys a box may be linked to. Sensitive kinds (`rrn`,
/// `account`, `account_holder`) are allowed as labels so the app can treat them as
/// enter-at-fill-time only; they are never prefilled.
pub const FIELD_KINDS: &[&str] = &[
    "name",
    "name_en",
    "birth_date",
    "phone",
    "address",
    "email",
    "school",
    "major",
    "grade",
    "student_id",
    "application_date",
    "rrn",
    "account",
    "bank",
    "account_holder",
    "signature",
    "stamp",
];
pub const DATE_FORMATS: &[&str] = &[
    "YYYY.MM.DD",
    "YYYY-MM-DD",
    "YYYY. MM. DD.",
    "YYYY년 MM월 DD일",
    "YYMMDD",
];

/// One part of a date printed as separate blanks. `year2`: the form prints "20"
/// before the blank ("20  년"), so the box takes the last two digits. Twin of
/// `DATE_PARTS` in apps/mobile/lib/formFill/fields.ts.
pub const DATE_PARTS: &[&str] = &["year2", "year4", "month", "day"];

/// Fractions of the page crop box, origin top-left, unrotated page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub page: i32,
    pub rect: Rect,
    #[serde(rename = "type")]
    pub field_type: String,
    pub label: String,
    #[serde(default)]
    pub mapping: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acro_name: Option<String>,
    /// [`Group`] id this checkbox is an option of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A text box holding one date part ([`DATE_PARTS`]). Absent on every
    /// existing template, which therefore keeps its exact bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

/// Checkbox options that answer one question. `single`: checking one
/// unchecks the others (radio); `multi`: independent ticks under one
/// heading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub mode: String,
    pub label: String,
}

pub const MAX_GROUPS: usize = 100;
pub const GROUP_MODES: &[&str] = &["single", "multi"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub version: i32,
    pub content_hash: String,
    pub page_count: i32,
    pub source: String,
    pub fields: Vec<Field>,
    /// Set when the official file is HWP/HWPX: `content_hash` is then the
    /// PDF Mazimi converted and serves itself, and the boxes are valid only
    /// while the official file still hashes to `source_sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<Derived>,
    /// Absent (and never serialized when empty), so templates without
    /// groups keep their exact bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
}

/// Provenance of a server-hosted PDF converted from an official HWP/HWPX.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Derived {
    /// sha256 of the official HWP/HWPX bytes the worker verified.
    pub source_sha256: String,
    /// Converter identity (tool, version, image digest), printable ASCII.
    pub converter: String,
}

/// Official formats Mazimi may convert to a self-hosted PDF.
pub const DERIVABLE_FORMATS: &[&str] = &["hwp", "hwpx"];
/// Same ceiling the app applies to a downloaded template.
pub const MAX_DERIVED_PDF_BYTES: usize = 8 * 1024 * 1024;

pub fn is_converter(value: &str) -> bool {
    (1..=200).contains(&value.len())
        && value.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !value.trim().is_empty()
}

/// Cheap structural check for an uploaded derived PDF: header and trailer.
pub fn looks_like_pdf(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(1024)..];
    bytes.starts_with(b"%PDF-") && tail.windows(5).any(|w| w == b"%%EOF")
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn is_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}
fn is_group_id(value: &str) -> bool {
    (1..=40).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}
/// Zero-width, bidi and other invisible formatting characters. A group
/// label is shown as a question heading, so it must read as printed. The
/// mobile twin is `INVISIBLE` in apps/mobile/lib/formFill/fields.ts.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}
fn is_text(value: &str, max: usize) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

/// The client capability that reads [`Field::part`] (header `X-Mazimi-Client-Caps`).
pub const DATE_PARTS_CAP: &str = "form-date-parts-v1";

impl Template {
    /// What a client that does not read `part` gets: date-part boxes keep no mapping and no part,
    /// exactly as before parts existed. Shipped builds suggest the full date ("2026.10.26") for an
    /// application_date box, so a 연/월/일 box would otherwise get the whole date.
    pub fn for_clients_without_date_parts(mut self) -> Self {
        for f in self.fields.iter_mut().filter(|f| f.part.is_some()) {
            f.part = None;
            f.mapping = None;
        }
        self
    }

    /// Structural rules shared with the mobile client. Messages are shown to admins.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("지원하지 않는 양식 정의 버전입니다.");
        }
        if !is_hash(&self.content_hash) {
            return Err("양식 파일 해시가 올바르지 않습니다.");
        }
        if !(1..=MAX_PAGES).contains(&self.page_count) {
            return Err("쪽수는 1~50쪽이어야 합니다.");
        }
        if let Some(derived) = &self.derived {
            if !is_hash(&derived.source_sha256) || derived.source_sha256 == self.content_hash {
                return Err("원본 양식 파일 해시가 올바르지 않습니다.");
            }
            if !is_converter(&derived.converter) {
                return Err("변환기 정보가 올바르지 않습니다.");
            }
        }
        if !matches!(self.source.as_str(), "admin" | "acroform") {
            return Err("정의 출처가 올바르지 않습니다.");
        }
        if self.fields.is_empty() || self.fields.len() > MAX_FIELDS {
            return Err("입력 칸은 1~200개여야 합니다.");
        }
        let mut ids = std::collections::HashSet::new();
        for field in &self.fields {
            if !is_id(&field.id) || !ids.insert(field.id.as_str()) {
                return Err("입력 칸 ID가 비었거나 중복됩니다.");
            }
            if !(0..self.page_count).contains(&field.page) {
                return Err("입력 칸의 쪽 번호가 양식 쪽수를 벗어납니다.");
            }
            let Rect { x, y, w, h } = field.rect;
            if ![x, y, w, h].iter().all(|v| v.is_finite())
                || x < 0.0
                || y < 0.0
                || w < 0.005
                || h < 0.005
                || x + w > 1.0001
                || y + h > 1.0001
            {
                return Err("입력 칸 위치가 쪽 밖에 있거나 너무 작습니다.");
            }
            if !FIELD_TYPES.contains(&field.field_type.as_str()) {
                return Err("입력 칸 종류가 올바르지 않습니다.");
            }
            if !is_text(&field.label, 80) {
                return Err("입력 칸 이름은 1~80자여야 합니다.");
            }
            if field
                .mapping
                .as_deref()
                .is_some_and(|m| !FIELD_KINDS.contains(&m))
            {
                return Err("연결 정보가 올바르지 않습니다.");
            }
            if field
                .format
                .as_deref()
                .is_some_and(|f| !DATE_FORMATS.contains(&f))
            {
                return Err("날짜 형식이 올바르지 않습니다.");
            }
            if field
                .part
                .as_deref()
                .is_some_and(|p| field.field_type != "text" || !DATE_PARTS.contains(&p))
            {
                return Err("날짜 부분은 글자 칸에만 year2/year4/month/day로 지정할 수 있습니다.");
            }
            if field
                .font_size
                .is_some_and(|s| !s.is_finite() || !(5.0..=24.0).contains(&s))
            {
                return Err("글자 크기는 5~24pt여야 합니다.");
            }
            if field
                .acro_name
                .as_deref()
                .is_some_and(|n| n.chars().count() > 200 || n.chars().any(char::is_control))
            {
                return Err("원본 입력 칸 이름이 올바르지 않습니다.");
            }
        }
        self.validate_groups()
    }

    fn validate_groups(&self) -> Result<(), &'static str> {
        if self.groups.len() > MAX_GROUPS {
            return Err("선택 묶음은 100개 이하여야 합니다.");
        }
        let mut ids = std::collections::HashSet::new();
        for group in &self.groups {
            if !is_group_id(&group.id) || !ids.insert(group.id.as_str()) {
                return Err("선택 묶음 ID가 올바르지 않거나 중복됩니다.");
            }
            if !GROUP_MODES.contains(&group.mode.as_str()) {
                return Err("선택 묶음 방식은 single 또는 multi여야 합니다.");
            }
            if !is_text(&group.label, 80) || group.label.chars().any(is_invisible) {
                return Err("선택 묶음 이름은 1~80자여야 합니다.");
            }
        }
        for field in &self.fields {
            let Some(group) = field.group.as_deref() else {
                continue;
            };
            if !ids.contains(group) {
                return Err("입력 칸이 없는 선택 묶음을 가리킵니다.");
            }
            if field.field_type != "checkbox" {
                return Err("선택 묶음에는 체크 칸만 넣을 수 있습니다.");
            }
        }
        for group in &self.groups {
            let pages: Vec<i32> = self
                .fields
                .iter()
                .filter(|f| f.group.as_deref() == Some(group.id.as_str()))
                .map(|f| f.page)
                .collect();
            if pages.len() < 2 {
                return Err("선택 묶음마다 선택지가 2개 이상이어야 합니다.");
            }
            if pages.iter().any(|p| *p != pages[0]) {
                return Err("한 선택 묶음의 선택지는 모두 같은 쪽에 있어야 합니다.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> Template {
        serde_json::from_value(serde_json::json!({
            "version": 1, "content_hash": "a".repeat(64), "page_count": 2, "source": "admin",
            "fields": [{"id":"f1","page":1,"rect":{"x":0.1,"y":0.2,"w":0.3,"h":0.04},"type":"text","label":"성명","mapping":"name","required":true},
                       {"id":"f2","page":0,"rect":{"x":0.6,"y":0.9,"w":0.2,"h":0.05},"type":"date","label":"신청일","mapping":"application_date","format":"YYYY년 MM월 DD일"}]
        }))
        .unwrap()
    }

    #[test]
    fn accepts_a_well_formed_definition() {
        assert_eq!(template().validate(), Ok(()));
    }

    #[test]
    fn a_date_part_is_additive_and_only_on_text_boxes() {
        let mut t = template();
        t.fields[0].part = Some("year2".into());
        assert_eq!(t.validate(), Ok(()));
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["fields"][0]["part"], "year2");
        assert!(
            json["fields"][1].get("part").is_none(),
            "absent stays absent"
        );
        t.fields[0].part = Some("year3".into());
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[1].part = Some("year2".into()); // a date box
        assert!(t.validate().is_err());
    }

    #[test]
    fn rejects_out_of_page_boxes_bad_types_and_duplicates() {
        let mut t = template();
        t.fields[0].page = 2;
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[0].rect.x = 0.9;
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[0].rect.w = f64::NAN;
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[0].field_type = "script".into();
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[1].id = "f1".into();
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[0].mapping = Some("password".into());
        assert!(t.validate().is_err());
        let mut t = template();
        t.fields[0].label = "성명\u{0}".into();
        assert!(t.validate().is_err());
        let mut t = template();
        t.content_hash = "A".repeat(64);
        assert!(
            t.validate().is_err(),
            "hash must be lowercase hex like core::digest"
        );
        let mut t = template();
        t.fields.clear();
        assert!(t.validate().is_err());
    }

    /// Templates prepared from official forms (scripts/forms/propose_fields.py)
    /// must parse and validate exactly as the admin PUT would.
    #[test]
    fn prepared_form_templates_validate() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/validation/form-templates");
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{} is committed: {e}", dir.display()));
        let mut checked = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.to_string_lossy().ends_with(".template.json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let template: Template =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(template.validate(), Ok(()), "{}", path.display());
            checked += 1;
        }
        assert!(checked > 0, "no *.template.json in {}", dir.display());
    }

    #[test]
    fn derived_templates_record_the_official_hash_and_converter() {
        let mut t = template();
        t.derived = Some(Derived {
            source_sha256: "b".repeat(64),
            converter: "rhwp 0.8.6 export-pdf; image sha256:abc".into(),
        });
        assert_eq!(t.validate(), Ok(()));
        let round: Template = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        assert_eq!(round, t);
        // A plain PDF template serializes exactly as before (no `derived` key).
        assert!(serde_json::to_value(template())
            .unwrap()
            .get("derived")
            .is_none());

        let bad = |f: &dyn Fn(&mut Derived)| {
            let mut t = t.clone();
            f(t.derived.as_mut().unwrap());
            t.validate()
        };
        assert!(bad(&|d| d.source_sha256 = "B".repeat(64)).is_err());
        assert!(
            bad(&|d| d.source_sha256 = "a".repeat(64)).is_err(),
            "source and PDF are different files"
        );
        assert!(bad(&|d| d.converter = String::new()).is_err());
        assert!(bad(&|d| d.converter = "rhwp\n0.8.6".into()).is_err());
        assert!(bad(&|d| d.converter = "변환기".into()).is_err());
        assert!(bad(&|d| d.converter = "x".repeat(201)).is_err());

        let mut value = serde_json::to_value(&t).unwrap();
        value["derived"]["pdf_url"] = serde_json::json!("https://evil.example/x.pdf");
        assert!(serde_json::from_value::<Template>(value).is_err());
    }

    fn grouped() -> Template {
        serde_json::from_value(serde_json::json!({
            "version": 1, "content_hash": "a".repeat(64), "page_count": 2, "source": "admin",
            "fields": [
                {"id":"f1","page":1,"rect":{"x":0.1,"y":0.2,"w":0.3,"h":0.04},"type":"text","label":"성명","mapping":"name","required":true},
                {"id":"c1","page":0,"rect":{"x":0.1,"y":0.5,"w":0.02,"h":0.02},"type":"checkbox","label":"구분: 대학생","mapping":null,"group":"g1"},
                {"id":"c2","page":0,"rect":{"x":0.3,"y":0.5,"w":0.02,"h":0.02},"type":"checkbox","label":"구분: 졸업생","mapping":null,"group":"g1"},
                {"id":"c3","page":0,"rect":{"x":0.1,"y":0.6,"w":0.02,"h":0.02},"type":"checkbox","label":"관심: 주거","mapping":null,"group":"g2"},
                {"id":"c4","page":0,"rect":{"x":0.3,"y":0.6,"w":0.02,"h":0.02},"type":"checkbox","label":"관심: 일자리","mapping":null,"group":"g2"}
            ],
            "groups": [{"id":"g1","mode":"single","label":"구분"},{"id":"g2","mode":"multi","label":"관심 분야"}]
        }))
        .unwrap()
    }

    #[test]
    fn checkbox_groups_validate_and_round_trip() {
        let t = grouped();
        assert_eq!(t.validate(), Ok(()));
        let round: Template = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        assert_eq!(round, t);
        let bad = |f: &dyn Fn(&mut Template)| {
            let mut t = grouped();
            f(&mut t);
            t.validate()
        };
        // Unknown group reference.
        assert!(bad(&|t| t.fields[1].group = Some("g9".into())).is_err());
        // One option only.
        assert!(bad(&|t| t.fields[2].group = None).is_err());
        // A group with no options at all.
        assert!(bad(&|t| t.groups.push(Group {
            id: "g3".into(),
            mode: "single".into(),
            label: "빈 묶음".into()
        }))
        .is_err());
        // Options on two pages.
        assert!(bad(&|t| t.fields[2].page = 1).is_err());
        // A non-checkbox in a group.
        assert!(bad(&|t| t.fields[0].group = Some("g1".into())).is_err());
        // Wrong mode, duplicate id, bad id, bad label, too many groups.
        assert!(bad(&|t| t.groups[0].mode = "radio".into()).is_err());
        assert!(bad(&|t| t.groups[1].id = "g1".into()).is_err());
        assert!(bad(&|t| {
            t.groups[0].id = "G1".into();
            t.fields[1].group = Some("G1".into());
            t.fields[2].group = Some("G1".into());
        })
        .is_err());
        assert!(bad(&|t| t.groups[0].id = "g".repeat(41)).is_err());
        assert!(bad(&|t| t.groups[0].label = " ".into()).is_err());
        assert!(bad(&|t| t.groups[0].label = "구\u{7}분".into()).is_err());
        assert!(bad(&|t| t.groups[0].label = "가".repeat(81)).is_err());
        for invisible in ["\u{FEFF}", "구\u{200B}분", "\u{202E}구분"] {
            assert!(
                bad(&|t| t.groups[0].label = invisible.into()).is_err(),
                "{invisible:?}"
            );
        }
        assert!(bad(&|t| {
            t.groups = (0..101)
                .map(|n| Group {
                    id: format!("g{n}"),
                    mode: "multi".into(),
                    label: "묶음".into(),
                })
                .collect();
        })
        .is_err());
        // Unknown keys inside a group are refused like everywhere else.
        let mut value = serde_json::to_value(grouped()).unwrap();
        value["groups"][0]["default"] = serde_json::json!("c1");
        assert!(serde_json::from_value::<Template>(value).is_err());
    }

    #[test]
    fn templates_without_groups_serialize_byte_identically() {
        // No `groups` or `group` key appears, so existing templates (and their
        // stored JSON) stay exactly as they were.
        let value = serde_json::to_value(template()).unwrap();
        assert!(value.get("groups").is_none());
        assert!(value["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f.get("group").is_none()));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/validation/form-templates");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if !path.to_string_lossy().ends_with(".template.json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let original: serde_json::Value = serde_json::from_str(&text).unwrap();
            let parsed: Template = serde_json::from_str(&text).unwrap();
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                original,
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn pdf_shape_check() {
        assert!(looks_like_pdf(b"%PDF-1.7\n1 0 obj\n%%EOF\n"));
        assert!(!looks_like_pdf(b"<html>%PDF-1.7 %%EOF"));
        assert!(!looks_like_pdf(b"%PDF-1.7 truncated"));
        assert!(!looks_like_pdf(b""));
    }

    #[test]
    fn unknown_keys_are_refused_at_parse_time() {
        let mut value = serde_json::to_value(template()).unwrap();
        value["fields"][0]["value"] = serde_json::json!("홍길동");
        assert!(
            serde_json::from_value::<Template>(value).is_err(),
            "no user values can be stored"
        );
    }

    #[test]
    fn clients_without_date_parts_get_no_mapping_on_part_boxes() {
        let mut t: Template = serde_json::from_value(serde_json::json!({
            "version": 1, "content_hash": "a".repeat(64), "page_count": 1, "source": "admin",
            "fields": [
                {"id": "y", "page": 0, "rect": {"x": 0.1, "y": 0.1, "w": 0.05, "h": 0.02}, "type": "text", "label": "신청일 연도", "mapping": "application_date", "required": false, "part": "year2"},
                {"id": "n", "page": 0, "rect": {"x": 0.1, "y": 0.2, "w": 0.3, "h": 0.02}, "type": "text", "label": "성명", "mapping": "name", "required": true},
                {"id": "d", "page": 0, "rect": {"x": 0.1, "y": 0.3, "w": 0.3, "h": 0.02}, "type": "text", "label": "작성일", "mapping": "application_date", "required": false}
            ]})).unwrap();
        t = t.for_clients_without_date_parts();
        assert_eq!(
            (t.fields[0].mapping.as_deref(), t.fields[0].part.as_deref()),
            (None, None)
        );
        assert_eq!(t.fields[1].mapping.as_deref(), Some("name"));
        assert_eq!(
            t.fields[2].mapping.as_deref(),
            Some("application_date"),
            "a whole-date box keeps its mapping"
        );
        assert!(t.validate().is_ok());
    }
}
