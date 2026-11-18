//! Versioned publisher ingestion. No remote HTML fetching or per-item review.
use crate::auth::AdminUser;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Duration, Utc};
use mazimi_core::source_policy::{canonicalize_public_https_url, host_is_within};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use subtle::ConstantTimeEq;
use uuid::Uuid;

type Error = (StatusCode, Json<Value>);
fn reject(status: StatusCode, message: &str) -> Error {
    (status, Json(json!({"error": message})))
}
fn invalid(message: &str) -> Error {
    reject(StatusCode::BAD_REQUEST, message)
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFeed {
    name: String,
    domain_id: Uuid,
    brand_id: Uuid,
    authority_reference: String,
    authorized_until: DateTime<Utc>,
}

/// The administrator records actual publication authority once per publisher.
/// This never enables crawling, user data transfer, or application submission.
pub async fn create(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<CreateFeed>,
) -> Result<Json<Value>, Error> {
    if !(1..=200).contains(&input.name.trim().chars().count())
        || !(10..=2000).contains(&input.authority_reference.trim().chars().count())
        || input.authorized_until <= Utc::now()
        || input.authorized_until > Utc::now() + Duration::days(366)
    {
        return Err(invalid(
            "제공자 이름, 데이터 제공 권한 근거, 366일 이내 만료일이 필요합니다.",
        ));
    }
    let verified = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM benefit_source_domains d JOIN brands b ON b.id = $2
         WHERE d.id = $1 AND d.is_active AND d.verification_status = 'verified' AND b.is_active)",
    )
    .bind(input.domain_id)
    .bind(input.brand_id)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    if !verified {
        return Err(invalid("검증된 공식 도메인과 활성 브랜드가 필요합니다."));
    }
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_feeds(name, domain_id, brand_id, authority_reference, authorized_until, token_hash, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(input.name.trim()).bind(input.domain_id).bind(input.brand_id)
        .bind(input.authority_reference.trim()).bind(input.authorized_until).bind(hash(token.as_bytes()))
        .bind(admin.0.id).fetch_one(&pool).await.map_err(crate::errors::internal_error)?;
    Ok(Json(json!({"id":id,"token":token,"schema_version":1})))
}

pub async fn revoke(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    let changed = sqlx::query("UPDATE benefit_feeds SET is_active = false WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?
        .rows_affected();
    if changed == 0 {
        return Err(reject(StatusCode::NOT_FOUND, "피드를 찾을 수 없습니다."));
    }
    sqlx::query("SELECT expire_benefit_feeds()")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Json(json!({"revoked":true})))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeedBatch {
    schema_version: u32,
    sequence: i64,
    items: Vec<Offer>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    external_id: String,
    title: String,
    summary: String,
    official_url: String,
    category: String,
    conditions: String,
    application_method: String,
    #[serde(default)]
    documents: Vec<String>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    /// Freshness expiry is mandatory even for an evergreen benefit.
    valid_until: DateTime<Utc>,
    enabled: bool,
}
#[derive(FromRow)]
struct Feed {
    brand_id: Uuid,
    brand_name: String,
    domain: String,
    allow_subdomains: bool,
    token_hash: String,
    last_sequence: i64,
    authorized_until: DateTime<Utc>,
}

fn validate(
    batch: &FeedBatch,
    domain: &str,
    subdomains: bool,
    until: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if batch.schema_version != 1 || batch.sequence < 1 || !(1..=100).contains(&batch.items.len()) {
        return Err(invalid(
            "schema_version=1, 양수 sequence, 1~100개 항목이 필요합니다.",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for offer in &batch.items {
        for (text, max) in [
            (&offer.external_id, 200),
            (&offer.title, 200),
            (&offer.summary, 1000),
            (&offer.conditions, 2000),
            (&offer.application_method, 2000),
        ] {
            if text.trim().is_empty()
                || text.chars().count() > max
                || text.contains('<')
                || text.chars().any(|c| c.is_control() && c != '\n')
            {
                return Err(invalid(
                    "필수 혜택 정보의 길이 또는 일반 텍스트 형식이 잘못되었습니다.",
                ));
            }
        }
        if !ids.insert(&offer.external_id) {
            return Err(invalid("동일한 external_id를 중복 전송할 수 없습니다."));
        }
        let url = canonicalize_public_https_url(&offer.official_url)
            .map_err(|_| invalid("공식 HTTPS URL이 필요합니다."))?;
        if !host_is_within(&url.domain, domain, subdomains) {
            return Err(invalid("등록된 공식 도메인 밖의 URL입니다."));
        }
        if ![
            "membership",
            "discount",
            "student_discount",
            "software",
            "subscription",
            "transport",
            "telecom",
            "finance",
            "education",
            "shopping",
            "other",
        ]
        .contains(&offer.category.as_str())
        {
            return Err(invalid("지원하지 않는 기업혜택 분류입니다."));
        }
        if offer.valid_until > now + Duration::days(90)
            || offer.valid_until > until
            || (offer.enabled && offer.valid_until <= now)
            || offer
                .starts_at
                .zip(offer.ends_at)
                .is_some_and(|(start, end)| start >= end)
            || (offer.enabled && offer.ends_at.is_some_and(|end| end <= now))
        {
            return Err(invalid("유효기간은 권한 만료일과 90일 이내여야 하며, 종료된 혜택은 enabled=false로 전송해야 합니다."));
        }
        if offer.documents.len() > 30
            || offer
                .documents
                .iter()
                .any(|d| d.trim().is_empty() || d.chars().count() > 200 || d.contains('<'))
        {
            return Err(invalid("서류 목록은 일반 텍스트 30개 이하여야 합니다."));
        }
    }
    Ok(())
}

/// PUT /api/v1/benefit-feeds/{id}/batches — feed-scoped bearer, not a user JWT.
pub async fn ingest(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(batch): Json<FeedBatch>,
) -> Result<Json<Value>, Error> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 64)
        .ok_or_else(|| reject(StatusCode::UNAUTHORIZED, "피드 인증이 필요합니다."))?;
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    let feed = sqlx::query_as::<_, Feed>(
        "SELECT f.brand_id, b.name AS brand_name, d.domain, d.allow_subdomains,
                f.token_hash, f.last_sequence, f.authorized_until
         FROM benefit_feeds f JOIN benefit_source_domains d ON d.id = f.domain_id JOIN brands b ON b.id = f.brand_id
         WHERE f.id = $1 AND f.is_active AND f.authorized_until > now()
           AND d.is_active AND d.verification_status = 'verified' AND b.is_active
         FOR UPDATE OF f")
        .bind(id).fetch_optional(&mut *tx).await.map_err(crate::errors::internal_error)?
        .ok_or_else(|| reject(StatusCode::UNAUTHORIZED,"피드 인증이 필요합니다."))?;
    if !bool::from(
        hash(token.as_bytes())
            .as_bytes()
            .ct_eq(feed.token_hash.as_bytes()),
    ) {
        return Err(reject(StatusCode::UNAUTHORIZED, "피드 인증이 필요합니다."));
    }
    let payload_hash =
        hash(&serde_json::to_vec(&batch).map_err(|_| invalid("잘못된 JSON입니다."))?);
    if batch.sequence <= feed.last_sequence {
        let previous = sqlx::query_scalar::<_, String>(
            "SELECT payload_hash FROM benefit_feed_batches WHERE feed_id=$1 AND sequence=$2",
        )
        .bind(id)
        .bind(batch.sequence)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
        return if previous.as_deref() == Some(&payload_hash) {
            Ok(Json(json!({"sequence":batch.sequence,"replayed":true})))
        } else {
            Err(reject(
                StatusCode::CONFLICT,
                "이미 처리된 sequence입니다. 변경에는 더 큰 sequence를 사용하세요.",
            ))
        };
    }
    validate(
        &batch,
        &feed.domain,
        feed.allow_subdomains,
        feed.authorized_until,
        Utc::now(),
    )?;
    let mut written = Vec::with_capacity(batch.items.len());
    for offer in &batch.items {
        let existing = sqlx::query_as::<_, (Uuid,bool)>("SELECT program_id, publication_suspended FROM benefit_feed_items WHERE feed_id=$1 AND external_id=$2 FOR UPDATE")
            .bind(id).bind(&offer.external_id).fetch_optional(&mut *tx).await.map_err(crate::errors::internal_error)?;
        let program_id = existing.map(|e| e.0).unwrap_or_else(Uuid::new_v4);
        written.push(program_id);
        let active = offer.enabled && !existing.is_some_and(|e| e.1);
        let status = if !offer.enabled {
            "closed"
        } else if offer.starts_at.is_some_and(|s| s > Utc::now()) {
            "upcoming"
        } else {
            "active"
        };
        let until = offer
            .ends_at
            .map_or(offer.valid_until, |end| end.min(offer.valid_until));
        // Only IDs owned by this feed can be updated. No upstream HTML/raw text is stored.
        sqlx::query("INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, company_name,
             provider_kind, official_url, benefit_category, application_method, submission_documents,
             application_start_at, application_end_at, benefit_valid_until, program_status, is_active,
             source_health, source_verified_at, source_review_due_at, normalized_payload, last_synced_at)
             VALUES ($1,'corporate_benefit','corporate_feed',$2,$3,$4,$5,$5,'corporate',$6,$7,$8,$9,$10,$11,$12,$13,$14,'current',now(),$12,$15,now())
             ON CONFLICT (id) DO UPDATE SET title=$3,summary=$4,official_url=$6,benefit_category=$7,application_method=$8,
             submission_documents=$9,application_start_at=$10,application_end_at=$11,benefit_valid_until=$12,
             program_status=$13,is_active=$14,source_health='current',source_verified_at=now(),source_review_due_at=$12,
             normalized_payload=$15,last_synced_at=now(),updated_at=now(),retired_at=NULL,retirement_reason=NULL")
            .bind(program_id).bind(format!("{id}:{}",offer.external_id)).bind(offer.title.trim()).bind(offer.summary.trim())
            .bind(&feed.brand_name).bind(&offer.official_url).bind(&offer.category).bind(&offer.application_method)
            .bind(offer.documents.join("\n")).bind(offer.starts_at).bind(offer.ends_at).bind(until).bind(status).bind(active)
            .bind(json!({"conditions":offer.conditions,"feed_id":id,"sequence":batch.sequence}))
            .execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
        sqlx::query("INSERT INTO program_brands(program_id,brand_id,relationship_type,is_primary) VALUES($1,$2,'redeemable_at',true) ON CONFLICT DO NOTHING")
            .bind(program_id).bind(feed.brand_id).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
        sqlx::query("DELETE FROM program_documents WHERE program_id=$1")
            .bind(program_id)
            .execute(&mut *tx)
            .await
            .map_err(crate::errors::internal_error)?;
        for (index, document) in offer.documents.iter().enumerate() {
            sqlx::query("INSERT INTO program_documents(program_id,document_name,is_required,sort_order) VALUES($1,$2,true,$3)")
                .bind(program_id).bind(document).bind(index as i32).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
        }
        sqlx::query("INSERT INTO benefit_feed_items(feed_id,external_id,program_id,last_sequence,publisher_enabled,valid_until)
            VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(feed_id,external_id) DO UPDATE SET last_sequence=$4,publisher_enabled=$5,valid_until=$6,updated_at=now()")
            .bind(id).bind(&offer.external_id).bind(program_id).bind(batch.sequence).bind(offer.enabled).bind(until)
            .execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    }
    // The offers' 기업혜택 chips, stored with the batch.
    let batches = written
        .len()
        .div_ceil(mazimi_core::corporate_categories::BATCH as usize)
        .max(1);
    mazimi_core::corporate_categories::refresh(&mut tx, Some(&written), batches)
        .await
        .map_err(crate::errors::internal_error)?;
    sqlx::query("INSERT INTO benefit_feed_batches(feed_id,sequence,payload_hash,item_count) VALUES($1,$2,$3,$4)")
        .bind(id).bind(batch.sequence).bind(payload_hash).bind(batch.items.len() as i32).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    sqlx::query("UPDATE benefit_feeds SET last_sequence=$2,last_received_at=now() WHERE id=$1")
        .bind(id)
        .bind(batch.sequence)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Json(
        json!({"sequence":batch.sequence,"accepted":batch.items.len(),"replayed":false}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn batch() -> FeedBatch {
        serde_json::from_value(json!({"schema_version":1,"sequence":1,"items":[{
            "external_id":"student-2026","title":"학생 멤버십","summary":"재학 확인 후 이용",
            "official_url":"https://example.com/students","category":"student_discount",
            "conditions":"재학생 인증 필요","application_method":"공식 홈페이지 신청","documents":[],
            "valid_until":Utc::now()+Duration::days(7),"enabled":true
        }]})).unwrap()
    }
    #[test]
    fn validates_facts_and_rejects_cross_domain_and_unbounded_freshness() {
        let mut data = batch();
        let now = Utc::now();
        let until = now + Duration::days(30);
        assert!(validate(&data, "example.com", false, until, now).is_ok());
        data.items[0].official_url = "https://example.com.evil.com/students".into();
        assert!(validate(&data, "example.com", true, until, now).is_err());
        data.items[0].official_url = "https://example.com/new-address".into();
        data.items[0].valid_until = now + Duration::days(91);
        assert!(validate(&data, "example.com", false, until, now).is_err());
    }
    #[test]
    fn unknown_fields_and_duplicate_ids_are_rejected() {
        let mut data = batch();
        let duplicate =
            serde_json::from_value(serde_json::to_value(&data.items[0]).unwrap()).unwrap();
        data.items.push(duplicate);
        assert!(validate(
            &data,
            "example.com",
            false,
            Utc::now() + Duration::days(30),
            Utc::now()
        )
        .is_err());
        assert!(serde_json::from_value::<FeedBatch>(
            json!({"schema_version":2,"sequence":1,"items":[],"publish_anything":true})
        )
        .is_err());
    }
}
