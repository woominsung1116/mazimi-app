//! A programme's required documents: the one source the detail page, the
//! preparation checklist, the vault match and the 비서's documents answer
//! read (QA 2026-10-31 B1: the 비서 quoted one passage of five documents).
//!
//! - Reviewed `program_documents` rows when the programme has any
//!   (`source: curated`);
//! - otherwise the notice's own documents field split into items
//!   ([`mazimi_core::documents::split`], `source: official_text`), with its
//!   `※` notes;
//! - [`RequiredDocuments::none_required`]: no items, and the notice states
//!   that nothing is to be submitted ([`mazimi_core::documents::states_none`]).
//!
//! The notice's documents text is kept as [`RequiredDocuments::text`]: the
//! evidence a documents answer quotes.
use mazimi_core::models::Program;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct RequiredDocument {
    pub id: Uuid,
    pub document_name: String,
    pub description: Option<String>,
    /// `Some(false)`: the notice marks it optional ("(선택)"); `None`: not
    /// stated (counted as required).
    pub is_required: Option<bool>,
    pub sort_order: Option<i32>,
    /// `curated` (a reviewed `program_documents` row) or `official_text`
    /// (split from the notice's own documents field).
    pub source: &'static str,
}

impl RequiredDocument {
    pub fn required(&self) -> bool {
        self.is_required != Some(false)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RequiredDocuments {
    pub items: Vec<RequiredDocument>,
    /// `※` notes of the documents text the list was split from.
    pub notes: Vec<String>,
    /// No items, and the notice's documents field states that nothing is to
    /// be submitted.
    pub none_required: bool,
    /// The notice's own documents text (blank: `None`).
    pub text: Option<String>,
}

/// The list split from the programme's own documents field, with its notes.
/// Ids are stable: derived from the programme and the item.
fn official(program: &Program) -> (Vec<RequiredDocument>, Vec<String>) {
    use sha2::{Digest, Sha256};
    let Some(list) = mazimi_core::documents::split(program.submission_documents.as_deref()) else {
        return (Vec::new(), Vec::new());
    };
    let items = list
        .items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let digest = Sha256::digest(format!("{}\0{index}\0{}", program.id, item.name));
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            RequiredDocument {
                id: uuid::Builder::from_sha1_bytes(bytes).into_uuid(),
                document_name: item.name,
                description: item.description,
                is_required: item.is_required,
                sort_order: Some(index as i32 + 1),
                source: "official_text",
            }
        })
        .collect();
    (items, list.notes)
}

/// The programme's required documents (module docs).
pub async fn load(
    connection: &mut sqlx::PgConnection,
    program: &Program,
) -> Result<RequiredDocuments, sqlx::Error> {
    let curated: Vec<RequiredDocument> =
        sqlx::query_as::<_, (Uuid, String, Option<String>, Option<bool>, Option<i32>)>(
            "SELECT id, document_name, description, is_required, sort_order
             FROM program_documents WHERE program_id = $1
             ORDER BY sort_order ASC, document_name ASC",
        )
        .bind(program.id)
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .map(
            |(id, document_name, description, is_required, sort_order)| RequiredDocument {
                id,
                document_name,
                description,
                is_required,
                sort_order,
                source: "curated",
            },
        )
        .collect();
    let (items, notes) = if curated.is_empty() {
        official(program)
    } else {
        (curated, Vec::new())
    };
    let text = program
        .submission_documents
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    Ok(RequiredDocuments {
        none_required: items.is_empty()
            && mazimi_core::documents::states_none(program.submission_documents.as_deref()),
        items,
        notes,
        text,
    })
}
