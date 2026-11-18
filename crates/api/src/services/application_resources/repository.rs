use super::{Queue, Review};
use mazimi_core::{
    application_resources::{fingerprint, Resource},
    models::Program,
};
use sqlx::PgConnection;
use uuid::Uuid;

/// Live rows first: rejected history never hides a reviewable candidate.
/// Live rows are capped at `MAX_RESOURCES`; the limit leaves room for history.
pub async fn list_on(db: &mut PgConnection, program: Uuid) -> Result<Vec<Resource>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM program_application_resources WHERE program_id=$1 ORDER BY (status='rejected'),verified_at DESC NULLS LAST,created_at,id LIMIT 100")
        .bind(program).fetch_all(db).await
}
pub async fn find(
    db: &mut PgConnection,
    program: Uuid,
    kind: &str,
    url: &str,
) -> Result<Option<Resource>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM program_application_resources WHERE program_id=$1 AND kind=$2 AND url=$3",
    )
    .bind(program)
    .bind(kind)
    .bind(url)
    .fetch_optional(db)
    .await
}
pub async fn live_count(db: &mut PgConnection, program: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM program_application_resources WHERE program_id=$1 AND status<>'rejected'",
    )
    .bind(program)
    .fetch_one(db)
    .await
}
/// An active, verified, approved and unexpired source-domain entry covers
/// `host`: exactly, or as a subdomain when the entry allows subdomains and
/// `exact` is false. An IDN host passes `exact`, so only an entry for that
/// very host approves it.
pub async fn approved_domain(
    db: &mut PgConnection,
    host: &str,
    exact: bool,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(mazimi_core::application_resources::APPROVED_DOMAIN_SQL)
        .bind(host)
        .bind(exact)
        .fetch_one(db)
        .await
}
/// The program's official_url was last written through the admin API
/// (and not since replaced by ingestion), so it cannot vouch for a domain.
pub async fn official_url_admin_set(db: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(admin_official_url = official_url, false) FROM programs WHERE id=$1",
    )
    .bind(id)
    .fetch_one(db)
    .await
}
pub async fn program(
    db: &mut PgConnection,
    id: Uuid,
    lock: bool,
) -> Result<Option<Program>, sqlx::Error> {
    sqlx::query_as(if lock {
        "SELECT * FROM programs WHERE id=$1 FOR UPDATE"
    } else {
        "SELECT * FROM programs WHERE id=$1"
    })
    .bind(id)
    .fetch_optional(db)
    .await
}
/// None when the same (program, kind, url) already exists.
pub async fn insert(
    db: &mut PgConnection,
    p: &Program,
    q: &Queue,
) -> Result<Option<Resource>, sqlx::Error> {
    sqlx::query_as("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint) VALUES($1,$2,$3,$4,$5,$6)
        ON CONFLICT(program_id,kind,url) DO NOTHING RETURNING *")
        .bind(p.id).bind(&q.kind).bind(q.url.trim()).bind(q.source_url.trim()).bind(q.label.trim()).bind(fingerprint(p)).fetch_optional(db).await
}
pub async fn resource_program(
    db: &mut PgConnection,
    id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT program_id FROM program_application_resources WHERE id=$1")
        .bind(id)
        .fetch_optional(db)
        .await
}
pub async fn get_locked(db: &mut PgConnection, id: Uuid) -> Result<Option<Resource>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM program_application_resources WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(db)
        .await
}
pub async fn save_review(
    db: &mut PgConnection,
    row: &Resource,
    p: &Program,
    actor: Uuid,
    input: &Review,
    hours: i64,
) -> Result<Resource, sqlx::Error> {
    let verified = input.decision == "verified";
    if verified && row.kind == "application" {
        // A new review deliberately replaces the previous preferred entry;
        // background checks must not alternate the user's destination.
        sqlx::query("UPDATE program_application_resources SET status='candidate',valid_until=NULL,check_error='superseded',revision=revision+1,updated_at=now() WHERE program_id=$1 AND kind='application' AND status='verified' AND id<>$2")
            .bind(p.id).bind(row.id).execute(&mut *db).await?;
    }
    // A review is a fresh decision against what the reviewer saw: it clears
    // the sticky `changed` observation, the last probe error and the failure
    // counters, and asks
    // the worker for a new baseline probe.
    let result = sqlx::query_as("UPDATE program_application_resources SET status=$2, access=$3,program_fingerprint=$4,reviewed_by=$5,review_note=$6,
        verified_at=CASE WHEN $7 THEN now() ELSE NULL END,valid_until=CASE WHEN $7 THEN now()+make_interval(hours=>$8) ELSE NULL END,
        check_status=NULL,check_error=NULL,last_failure_code=NULL,consecutive_failures=0,consecutive_gone=0,
        next_check_at=now(),revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *")
        .bind(row.id).bind(&input.decision).bind(input.access.as_deref().unwrap_or("unknown")).bind(fingerprint(p))
        .bind(actor).bind(input.note.trim()).bind(verified).bind(hours as i32).fetch_one(&mut *db).await?;
    sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,$3,$4,$5)")
        .bind(row.id).bind(actor).bind(&input.decision).bind(row.revision)
        .bind(serde_json::json!({"program_fingerprint":fingerprint(p),"url":row.url,"source_url":row.source_url,"content_hash":row.content_hash,
            "access":input.access,"note":input.note,"validity_hours":hours,"same_program":input.same_program_confirmed,"current_intake":input.current_intake_confirmed,"destination_confirmed":input.destination_confirmed}))
        .execute(db).await?;
    Ok(result)
}
