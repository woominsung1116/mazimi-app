//! Evidence boundaries for multi-benefit planning. No inferred entitlement,
//! invented interest rate, additive payout, or implicit preparation mutations.
use super::{
    application_preparation::Error,
    benefit_assistant::{self, Guidance},
};
use crate::assistant_provider::AnswerMode;
use axum::{http::StatusCode, Json};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

pub const STACK_NOTICE: &str = "중복 수혜를 판정할 공식 근거가 등록되지 않았어요. 기관이나 유형이 같거나 다르다는 이유만으로 가능·불가를 판단하지 않아요. 각 공식 공고의 중복 제한을 확인해 주세요.";
pub const DOCUMENT_NOTICE: &str = "이름이 같은 서류의 준비 후보예요. 발급일·수신 기관·제출 형식·필수 여부는 혜택별로 확인해야 해요. 같은 파일을 재사용할 수 있다는 뜻이 아니며 준비 완료로 자동 처리하지 않아요.";

#[derive(Serialize)]
pub struct Pair {
    pub program_a_id: Uuid,
    pub program_a_title: String,
    pub program_b_id: Uuid,
    pub program_b_title: String,
    pub status: &'static str,
    pub reason: &'static str,
}
#[derive(Serialize, sqlx::FromRow)]
pub struct Summary {
    pub program_id: Uuid,
    pub title: String,
    pub program_type: String,
    pub provider_name: Option<String>,
    pub status: String,
}
#[derive(Serialize)]
pub struct StackCheck {
    pub programs: Vec<Summary>,
    pub pairs: Vec<Pair>,
    /// False means NOT established, not "incompatible". Inspect pair.status.
    pub all_stackable: bool,
    pub notice: &'static str,
}
#[derive(Serialize)]
pub struct DocumentUse {
    pub program_id: Uuid,
    pub document_id: String,
    pub required: Option<bool>,
    pub evidence: String,
    pub source: &'static str,
}
#[derive(Serialize)]
pub struct SharedDocument {
    pub name: String,
    pub uses: Vec<DocumentUse>,
}
#[derive(Serialize)]
pub struct Comparison {
    pub version: u8,
    pub programs: Vec<Guidance>,
    pub pairs: Vec<Pair>,
    pub shared_documents: Vec<SharedDocument>,
    pub compatibility_notice: &'static str,
    pub documents_notice: &'static str,
    pub amounts_additive: bool,
}

/// Deposits, loans and other financial products are not compared or bundled
/// until a legal review of 금융소비자보호법 platform rules clears it. Same type
/// list the preparation checklist treats as financial, plus a `finance`
/// category (enrichment may assign it to a corporate benefit such as a card).
/// Never NULL: callers decode it as a bool.
pub(crate) const FINANCIAL_PRODUCT_SQL: &str = "(program_type IN ('financial_product','loan','finance','savings','financial') OR source_type = 'fss_financial' OR benefit_category IS NOT DISTINCT FROM 'finance')";

pub fn validate_ids(ids: &[Uuid], max: usize) -> Result<Vec<Uuid>, Error> {
    let mut seen = HashSet::new();
    let unique: Vec<_> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    if ids.len() > max || unique.len() < 2 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("서로 다른 혜택을 2~{max}개 선택해 주세요.")})),
        ));
    }
    Ok(unique)
}

fn pairs(programs: &[(Uuid, &str)]) -> Vec<Pair> {
    let mut result = Vec::new();
    for (i, (a, title_a)) in programs.iter().enumerate() {
        for (b, title_b) in &programs[i + 1..] {
            result.push(Pair {
                program_a_id: *a,
                program_a_title: (*title_a).into(),
                program_b_id: *b,
                program_b_title: (*title_b).into(),
                status: "unknown",
                reason: STACK_NOTICE,
            });
        }
    }
    result
}

pub async fn stack_check(pool: &PgPool, ids: &[Uuid]) -> Result<StackCheck, Error> {
    let ids = validate_ids(ids, 20)?;
    let mut rows: HashMap<_, _> = sqlx::query_as::<_, Summary>(&format!(
        "SELECT id AS program_id, title, program_type, provider_name, 'unknown'::text AS status
         FROM current_programs WHERE id = ANY($1) AND {}",
        mazimi_core::currently_available_sql("", "now()")
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(crate::errors::internal_error)?
    .into_iter()
    .map(|p| (p.program_id, p))
    .collect();
    if rows.len() != ids.len() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(
                json!({"error":"선택한 혜택 중 현재 비교할 수 없는 항목이 있어요. 목록을 새로 확인해 주세요."}),
            ),
        ));
    }
    let programs: Vec<_> = ids.iter().filter_map(|id| rows.remove(id)).collect();
    let pairs = pairs(
        &programs
            .iter()
            .map(|p| (p.program_id, p.title.as_str()))
            .collect::<Vec<_>>(),
    );
    Ok(StackCheck {
        programs,
        pairs,
        all_stackable: false,
        notice: STACK_NOTICE,
    })
}

pub async fn compare(pool: &PgPool, user: Uuid, ids: &[Uuid]) -> Result<Comparison, Error> {
    let ids = validate_ids(ids, 4)?;
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let financial: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM programs WHERE id = ANY($1) AND {FINANCIAL_PRODUCT_SQL})"
    ))
    .bind(&ids)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    if financial {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":"예금·대출 같은 금융상품은 비교할 수 없어요. 각 상품의 공식 안내를 확인해 주세요."}),
            ),
        ));
    }
    let mut programs = Vec::new();
    for id in &ids {
        programs.push(
            benefit_assistant::guidance_on(&mut tx, user, *id, &[], AnswerMode::Rules, false, None)
                .await?,
        );
    }
    // Registered document rows only. Free-text `submission_documents` is not
    // split into candidates: a guessed name is not a document the user holds.
    let document_rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<bool>)>(
        "SELECT program_id, document_name, description, is_required
         FROM program_documents WHERE program_id = ANY($1)
         ORDER BY array_position($1, program_id), sort_order, document_name",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    let pairs = pairs(
        &programs
            .iter()
            .map(|p| (p.program_id, p.title.as_str()))
            .collect::<Vec<_>>(),
    );
    let mut documents: BTreeMap<String, SharedDocument> = BTreeMap::new();
    for (program_id, name, description, required) in document_rows {
        // Whitespace/case only: no inferred equivalence between types.
        let key = name.split_whitespace().collect::<String>().to_lowercase();
        if key.is_empty() {
            continue;
        }
        let entry = documents.entry(key).or_insert_with(|| SharedDocument {
            name: name.clone(),
            uses: vec![],
        });
        entry.uses.push(DocumentUse {
            program_id,
            // Same id as the preparation task so a client can link the two.
            document_id: format!("document:{name}"),
            required,
            evidence: description.unwrap_or_else(|| name.clone()),
            source: "program_documents",
        });
    }
    let shared_documents = documents
        .into_values()
        .filter(|doc| {
            doc.uses
                .iter()
                .map(|usage| usage.program_id)
                .collect::<HashSet<_>>()
                .len()
                >= 2
        })
        .collect();
    Ok(Comparison {
        version: 1,
        programs,
        pairs,
        shared_documents,
        compatibility_notice: STACK_NOTICE,
        documents_notice: DOCUMENT_NOTICE,
        amounts_additive: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_pair_is_inferred_compatible_or_exclusive() {
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let result = pairs(&[
            (ids[0], "장학금"),
            (ids[1], "다른 기관 장학금"),
            (ids[2], "대출"),
        ]);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|p| p.status == "unknown"));
        assert!(validate_ids(&[ids[0], ids[0]], 4).is_err());
        assert_eq!(
            validate_ids(&[ids[0], ids[1], ids[0]], 4).unwrap(),
            ids[..2]
        );
    }
}
