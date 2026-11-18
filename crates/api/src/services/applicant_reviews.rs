//! Serve-time reading of `program_applicant_reviews`. See
//! [`mazimi_core::applicant_review`].

use std::collections::HashMap;

use mazimi_core::applicant_review::{self, ApplicantClass, ReviewEffect};
use serde_json::Value;
use uuid::Uuid;

/// Effects of the hiding/demoting reviews of current programmes, keyed by
/// programme id; `ids = None` reads every reviewed row. Rows whose review is
/// stale or has no effect are left out.
pub(crate) async fn effects(
    connection: &mut sqlx::PgConnection,
    ids: Option<&[Uuid]>,
) -> Result<HashMap<Uuid, ReviewEffect>, sqlx::Error> {
    // Only the hashed fields of the notice, never the whole payload (audit
    // M4): the applicant hash reads the reviewed text fields, a subset.
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, Option<Value>)>(&format!(
        "SELECT p.id, p.source_type, r.class, r.text_hash, {hashed}
         FROM program_applicant_reviews r
         JOIN current_programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         WHERE r.class IN ('organisation', 'business', 'indirect')
           AND ($1::uuid[] IS NULL OR p.id = ANY($1))",
        hashed = mazimi_core::condition_review::hashed_projection_sql("p."),
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, source_type, class, stored, raw)| {
            let class = ApplicantClass::parse(&class)?;
            let current =
                applicant_review::text_hash(&source_type, raw.as_ref().unwrap_or(&Value::Null));
            match applicant_review::effect(class, &stored, current.as_deref()) {
                ReviewEffect::None => None,
                effect => Some((id, effect)),
            }
        })
        .collect())
}
