//! 서류 작성 도우미 field definitions. Admins draw input boxes on a verified
//! PDF template; the public read serves them only while the template is
//! currently published and its bytes still match the hash the boxes were
//! drawn on. No user answers or filled documents ever reach the server.
//!
//! An official HWP/HWPX cannot be overlaid by the app, so an admin uploads a
//! PDF converted from it and Mazimi serves that PDF itself. The boxes are then
//! bound twice: to the converted PDF (`content_hash`, checked by the app after
//! download) and to the official file it came from (`derived.source_sha256`,
//! which must equal the worker-verified resource hash). A changed official
//! file therefore hides both the PDF and the boxes, as for a PDF template.
use crate::services::application_resources::{self, Error, PublicResource};
use mazimi_core::{
    application_resources::digest,
    form_templates::{
        is_converter, looks_like_pdf, Derived, Template, DERIVABLE_FORMATS, MAX_DERIVED_PDF_BYTES,
    },
    models::Program,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, PgPool};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Put {
    pub template: Template,
    /// Optimistic lock: the revision the admin edited, or None for a new definition.
    pub expected_revision: Option<i64>,
}

/// Query of the derived PDF upload; the body is the raw PDF.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedUpload {
    pub source_sha256: String,
    pub converter: String,
    /// Optimistic lock: the PDF hash the admin saw. Required to replace a PDF
    /// that live boxes are drawn on, since replacing it hides those boxes.
    #[serde(default)]
    pub expected_content_hash: Option<String>,
}

#[derive(sqlx::FromRow)]
struct TemplateResource {
    kind: String,
    file_format: Option<String>,
    content_hash: Option<String>,
    label: String,
    url: String,
}
impl TemplateResource {
    fn derivable(&self) -> bool {
        self.file_format
            .as_deref()
            .is_some_and(|f| DERIVABLE_FORMATS.contains(&f))
    }
}
#[derive(sqlx::FromRow)]
struct Stored {
    content_hash: String,
    page_count: i32,
    source: String,
    fields: SqlJson<Value>,
    groups: SqlJson<Value>,
    source_sha256: Option<String>,
    converter: Option<String>,
    revision: i64,
    updated_at: chrono::DateTime<chrono::Utc>,
}
impl Stored {
    fn template(self) -> Result<Template, Error> {
        let derived = match (self.source_sha256, self.converter) {
            (Some(source_sha256), Some(converter)) => Some(Derived {
                source_sha256,
                converter,
            }),
            _ => None,
        };
        serde_json::from_value(
            json!({"version":1,"content_hash":self.content_hash,"page_count":self.page_count,
            "source":self.source,"fields":self.fields.0,"groups":self.groups.0,"derived":derived}),
        )
        .map_err(|e| Error::Database(sqlx::Error::Decode(Box::new(e))))
    }
}
#[derive(sqlx::FromRow)]
struct DerivedPdf {
    source_sha256: String,
    converter: String,
    content_hash: String,
    byte_size: i32,
    created_at: chrono::DateTime<chrono::Utc>,
}

const STORED: &str = "SELECT content_hash,page_count,source,fields,groups,source_sha256,converter,revision,updated_at FROM application_form_templates WHERE resource_id=$1";
const DERIVED: &str = "SELECT source_sha256,converter,content_hash,byte_size,created_at FROM application_form_derived_pdfs WHERE resource_id=$1";

/// Whether stored boxes still belong to the resource's current official file.
fn current(resource_hash: Option<&str>, stored: &Stored, pdf: Option<&DerivedPdf>) -> bool {
    match stored.source_sha256.as_deref() {
        None => resource_hash == Some(stored.content_hash.as_str()),
        Some(source) => {
            resource_hash == Some(source)
                && pdf.is_some_and(|p| {
                    p.source_sha256 == source
                        && p.content_hash == stored.content_hash
                        && Some(p.converter.as_str()) == stored.converter.as_deref()
                })
        }
    }
}

pub async fn admin_get(pool: &PgPool, resource: Uuid) -> Result<Value, Error> {
    let mut db = pool.acquire().await?;
    let row: TemplateResource = sqlx::query_as("SELECT kind,file_format,content_hash,label,url FROM program_application_resources WHERE id=$1")
        .bind(resource).fetch_optional(&mut *db).await?.ok_or(Error::NotFound)?;
    let stored: Option<Stored> = sqlx::query_as(STORED)
        .bind(resource)
        .fetch_optional(&mut *db)
        .await?;
    let pdf: Option<DerivedPdf> = sqlx::query_as(DERIVED)
        .bind(resource)
        .fetch_optional(&mut *db)
        .await?;
    let (template, revision, updated_at, stale) = match stored {
        Some(s) => {
            let stale = !current(row.content_hash.as_deref(), &s, pdf.as_ref());
            let (revision, updated_at) = (s.revision, s.updated_at);
            (Some(s.template()?), Some(revision), Some(updated_at), stale)
        }
        None => (None, None, None, false),
    };
    let derived_pdf = pdf.map(|p| {
        json!({"content_hash":p.content_hash,"source_sha256":p.source_sha256,"converter":p.converter,
            "byte_size":p.byte_size,"created_at":p.created_at,
            "stale": row.content_hash.as_deref() != Some(p.source_sha256.as_str())})
    });
    Ok(json!({
        "resource": {"id":resource,"kind":row.kind,"file_format":row.file_format,"content_hash":row.content_hash,"label":row.label,"url":row.url},
        "template": template, "revision": revision, "updated_at": updated_at,
        // The official file changed after these boxes were drawn: they are not served.
        "stale": stale,
        "derived_pdf": derived_pdf,
    }))
}

/// Loads the resource for a write and applies the rules every write shares.
async fn writable(tx: &mut sqlx::PgConnection, resource: Uuid) -> Result<TemplateResource, Error> {
    let row: TemplateResource = sqlx::query_as("SELECT kind,file_format,content_hash,label,url FROM program_application_resources WHERE id=$1 FOR UPDATE")
        .bind(resource).fetch_optional(&mut *tx).await?.ok_or(Error::NotFound)?;
    if row.kind != "template" || !(row.file_format.as_deref() == Some("pdf") || row.derivable()) {
        return Err(Error::Invalid(
            "수집기가 확인한 PDF·HWP 제출 양식에만 입력 칸을 정의할 수 있습니다.",
        ));
    }
    // Financial-product applications are out of scope for the form assistant.
    let financial: bool = sqlx::query_scalar("SELECT p.program_type='financial_product' FROM programs p JOIN program_application_resources r ON r.program_id=p.id WHERE r.id=$1")
        .bind(resource).fetch_one(&mut *tx).await?;
    if financial {
        return Err(Error::Invalid(
            "금융상품 신청서는 서류 작성 도우미 대상이 아닙니다.",
        ));
    }
    Ok(row)
}

/// Stores the PDF converted from the resource's verified HWP/HWPX. It is
/// accepted only for the official bytes the worker verified right now.
pub async fn admin_put_derived_pdf(
    pool: &PgPool,
    admin: Uuid,
    resource: Uuid,
    meta: DerivedUpload,
    pdf: &[u8],
) -> Result<Value, Error> {
    if pdf.is_empty() || pdf.len() > MAX_DERIVED_PDF_BYTES || !looks_like_pdf(pdf) {
        return Err(Error::Invalid("8MB 이하의 PDF 파일만 올릴 수 있습니다."));
    }
    if !is_converter(&meta.converter) {
        return Err(Error::Invalid("변환기 정보가 올바르지 않습니다."));
    }
    let content_hash = digest(pdf);
    let mut tx = pool.begin().await?;
    let row = writable(&mut tx, resource).await?;
    if !row.derivable() {
        return Err(Error::Invalid(
            "변환 PDF는 HWP·HWPX 공식 양식에만 올릴 수 있습니다.",
        ));
    }
    // Converted from other bytes than the worker verified: the copy would not
    // be the official form.
    if row.content_hash.as_deref() != Some(meta.source_sha256.as_str())
        || content_hash == meta.source_sha256
    {
        return Err(Error::Conflict);
    }
    // Boxes currently served on the stored PDF: replacing it (other bytes or
    // another converter) takes them offline, so the caller must say which PDF
    // it meant to replace.
    let live: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT t.content_hash,t.converter FROM application_form_templates t
         JOIN application_form_derived_pdfs p ON p.resource_id=t.resource_id
         WHERE t.resource_id=$1 AND t.source_sha256=$2 AND p.source_sha256=$2
           AND p.content_hash=t.content_hash AND p.converter=t.converter
         FOR UPDATE OF t, p",
    )
    .bind(resource)
    .bind(&meta.source_sha256)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((live_hash, live_converter)) = live {
        let replaces =
            live_hash != content_hash || live_converter.as_deref() != Some(meta.converter.as_str());
        if replaces && meta.expected_content_hash.as_deref() != Some(live_hash.as_str()) {
            return Err(Error::Conflict);
        }
    }
    sqlx::query(
        "INSERT INTO application_form_derived_pdfs(resource_id,source_sha256,converter,content_hash,byte_size,pdf,created_by)
         VALUES($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (resource_id) DO UPDATE SET source_sha256=EXCLUDED.source_sha256,converter=EXCLUDED.converter,
           content_hash=EXCLUDED.content_hash,byte_size=EXCLUDED.byte_size,pdf=EXCLUDED.pdf,
           created_by=EXCLUDED.created_by,created_at=now()",
    )
    .bind(resource)
    .bind(&meta.source_sha256)
    .bind(&meta.converter)
    .bind(&content_hash)
    .bind(pdf.len() as i32)
    .bind(pdf)
    .bind(admin)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(%resource, %admin, bytes = pdf.len(), "derived form pdf saved");
    Ok(json!({"content_hash": content_hash, "byte_size": pdf.len()}))
}

pub async fn admin_put(
    pool: &PgPool,
    admin: Uuid,
    resource: Uuid,
    input: Put,
) -> Result<Value, Error> {
    input.template.validate().map_err(Error::Invalid)?;
    let mut tx = pool.begin().await?;
    let row = writable(&mut tx, resource).await?;
    let t = &input.template;
    match (&t.derived, row.derivable()) {
        // Boxes drawn on other bytes than the worker verified would misplace answers.
        (None, false) => {
            if row.content_hash.as_deref() != Some(t.content_hash.as_str()) {
                return Err(Error::Conflict);
            }
        }
        // Derived boxes must sit on the uploaded PDF converted from the current official file.
        (Some(d), true) => {
            let pdf: Option<DerivedPdf> = sqlx::query_as(&format!("{DERIVED} FOR UPDATE"))
                .bind(resource)
                .fetch_optional(&mut *tx)
                .await?;
            let bound = row.content_hash.as_deref() == Some(d.source_sha256.as_str())
                && pdf.is_some_and(|p| {
                    p.source_sha256 == d.source_sha256
                        && p.converter == d.converter
                        && p.content_hash == t.content_hash
                });
            if !bound {
                return Err(Error::Conflict);
            }
        }
        (None, true) => {
            return Err(Error::Invalid(
                "HWP 양식은 변환 PDF 출처(derived)를 함께 보내야 합니다.",
            ))
        }
        (Some(_), false) => {
            return Err(Error::Invalid(
                "PDF 양식에는 변환 출처(derived)를 붙일 수 없습니다.",
            ))
        }
    }
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM application_form_templates WHERE resource_id=$1 FOR UPDATE",
    )
    .bind(resource)
    .fetch_optional(&mut *tx)
    .await?;
    if current != input.expected_revision {
        return Err(Error::Conflict);
    }
    let (source_sha256, converter) = match &t.derived {
        Some(d) => (Some(&d.source_sha256), Some(&d.converter)),
        None => (None, None),
    };
    let saved: (i64,) = sqlx::query_as(
        "INSERT INTO application_form_templates(resource_id,content_hash,page_count,source,fields,updated_by,source_sha256,converter,groups)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
         ON CONFLICT (resource_id) DO UPDATE SET content_hash=EXCLUDED.content_hash,page_count=EXCLUDED.page_count,
           source=EXCLUDED.source,fields=EXCLUDED.fields,groups=EXCLUDED.groups,updated_by=EXCLUDED.updated_by,
           source_sha256=EXCLUDED.source_sha256,converter=EXCLUDED.converter,
           revision=application_form_templates.revision+1,updated_at=now()
         RETURNING revision",
    )
    .bind(resource)
    .bind(&t.content_hash)
    .bind(t.page_count)
    .bind(&t.source)
    .bind(SqlJson(&t.fields))
    .bind(admin)
    .bind(source_sha256)
    .bind(converter)
    .bind(SqlJson(&t.groups))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(%resource, %admin, fields = t.fields.len(), revision = saved.0, derived = t.derived.is_some(), "form template saved");
    Ok(json!({"revision": saved.0}))
}

pub async fn admin_delete(pool: &PgPool, admin: Uuid, resource: Uuid) -> Result<(), Error> {
    let deleted = sqlx::query("DELETE FROM application_form_templates WHERE resource_id=$1")
        .bind(resource)
        .execute(pool)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    tracing::info!(%resource, %admin, "form template deleted");
    Ok(())
}

/// The template resource users can see right now, under the publication rule
/// `application_policy.resources` uses, or None.
async fn published(
    db: &mut sqlx::PgConnection,
    program: &Program,
    resource: Uuid,
) -> Result<Option<PublicResource>, Error> {
    if program.program_type == "financial_product" {
        return Ok(None);
    }
    let resources = application_resources::load(db, program).await?;
    Ok(resources.templates.into_iter().find(|r| r.id == resource))
}

/// The definition for a template users can see right now, or None. A stale,
/// expired or demoted template never exposes boxes.
pub async fn public_get(
    pool: &PgPool,
    program: &Program,
    resource: Uuid,
) -> Result<Option<Template>, Error> {
    let mut db = pool.acquire().await?;
    let Some(published) = published(&mut db, program, resource).await? else {
        return Ok(None);
    };
    let derivable = published
        .file_format
        .as_deref()
        .is_some_and(|f| DERIVABLE_FORMATS.contains(&f));
    if published.file_format.as_deref() != Some("pdf") && !derivable {
        return Ok(None);
    }
    let Some(stored) = sqlx::query_as::<_, Stored>(STORED)
        .bind(resource)
        .fetch_optional(&mut *db)
        .await?
    else {
        return Ok(None);
    };
    // A PDF resource never serves derived boxes and an HWP resource only does.
    if derivable != stored.source_sha256.is_some() {
        return Ok(None);
    }
    let pdf: Option<DerivedPdf> = if derivable {
        sqlx::query_as(DERIVED)
            .bind(resource)
            .fetch_optional(&mut *db)
            .await?
    } else {
        None
    };
    if !current(published.content_hash.as_deref(), &stored, pdf.as_ref()) {
        return Ok(None);
    }
    stored.template().map(Some)
}

/// Hash of the PDF converted from a published HWP/HWPX template, only while
/// it belongs to the current official file and boxes are defined on it.
/// Checked before any bytes are loaded, so a conditional GET costs no PDF read.
pub async fn public_derived_pdf_hash(
    pool: &PgPool,
    program: &Program,
    resource: Uuid,
) -> Result<Option<String>, Error> {
    Ok(public_get(pool, program, resource)
        .await?
        .filter(|template| template.derived.is_some())
        .map(|template| template.content_hash))
}

/// The bytes for a hash from [`public_derived_pdf_hash`]. Keyed by the hash,
/// so the bytes always match what the app verifies.
pub async fn derived_pdf_bytes(
    pool: &PgPool,
    resource: Uuid,
    content_hash: &str,
) -> Result<Option<Vec<u8>>, Error> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as(
        "SELECT pdf FROM application_form_derived_pdfs WHERE resource_id=$1 AND content_hash=$2",
    )
    .bind(resource)
    .bind(content_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(pdf,)| pdf))
}
