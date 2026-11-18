//! Import reviewed applicant classes from JSON lines into
//! `program_applicant_reviews`. Each line is
//! `{"source_type","source_id","class","evidence","reviewer","text_hash", …}`; other keys
//! (such as `title`) are annotations for people and are ignored.
//!
//! Every line must carry `text_hash`, the hash of the text the curators
//! read; it must equal the hash of the programme's CURRENT `raw_payload`
//! (else `text_changed`), and the evidence must be a verbatim quote of one
//! reviewed field. All-or-nothing: if any line is rejected, nothing is
//! written; `--dry-run` never writes.

use std::collections::{BTreeMap, HashSet};

use mazimi_core::applicant_review::{self, Review, Written};
use serde::Serialize;
use sqlx::Connection;

#[derive(Debug, Default, Serialize)]
pub struct ImportSummary {
    pub dry_run: bool,
    /// Whether the writes were committed.
    pub applied: bool,
    pub lines: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub rejected: usize,
    pub rejected_by_reason: BTreeMap<String, usize>,
    pub accepted_by_class: BTreeMap<String, usize>,
    /// Rejected lines (1-based line number, source id, reason). No source
    /// text is echoed.
    pub rejections: Vec<LineRejection>,
}

#[derive(Debug, Serialize)]
pub struct LineRejection {
    pub line: usize,
    pub source_id: Option<String>,
    pub reason: String,
}

/// The log actor of an import: `import:<operator>:<source>`. The operator
/// is `--operator`, else `$USER`; one of them is required.
pub fn import_actor(
    operator: Option<&str>,
    user: Option<String>,
    source: &str,
) -> Result<String, &'static str> {
    let operator = match operator {
        Some(operator) => operator.trim().to_owned(),
        None => user.unwrap_or_default().trim().to_owned(),
    };
    if !(1..=100).contains(&operator.chars().count()) || operator.chars().any(char::is_control) {
        return Err("set --operator <name> (1-100 characters) or $USER");
    }
    let source: String = source
        .chars()
        .filter(|c| !c.is_control())
        .take(150)
        .collect();
    Ok(format!("import:{operator}:{source}"))
}

/// Import `text` (JSON lines) in a transaction of `connection`.
pub async fn import_on(
    connection: &mut sqlx::PgConnection,
    text: &str,
    actor: &str,
    dry_run: bool,
) -> Result<ImportSummary, sqlx::Error> {
    let mut summary = ImportSummary {
        dry_run,
        ..ImportSummary::default()
    };
    let mut seen = HashSet::new();
    let mut tx = connection.begin().await?;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        summary.lines += 1;
        let mut reject = |source_id: Option<String>, reason: String| {
            *summary
                .rejected_by_reason
                .entry(reason.clone())
                .or_default() += 1;
            summary.rejected += 1;
            summary.rejections.push(LineRejection {
                line: index + 1,
                source_id,
                reason,
            });
        };
        let review: Review = match serde_json::from_str(line) {
            Ok(review) => review,
            Err(_) => {
                reject(None, "invalid_json".into());
                continue;
            }
        };
        let key = (
            review.source_type.trim().to_owned(),
            review.source_id.trim().to_owned(),
        );
        if !seen.insert(key) {
            reject(Some(review.source_id), "duplicate".into());
            continue;
        }
        match applicant_review::upsert(&mut tx, &review, actor, true).await? {
            Ok(written) => {
                *summary
                    .accepted_by_class
                    .entry(review.class.trim().to_owned())
                    .or_default() += 1;
                match written {
                    Written::Inserted => summary.inserted += 1,
                    Written::Updated => summary.updated += 1,
                    Written::Unchanged => summary.unchanged += 1,
                }
            }
            Err(rejection) => {
                let reason = serde_json::to_value(rejection)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{rejection:?}"));
                reject(Some(review.source_id), reason);
            }
        }
    }
    if dry_run || summary.rejected > 0 {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
        summary.applied = true;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn connection() -> sqlx::PgConnection {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::PgConnection::connect(&url)
            .await
            .expect("isolated test database required")
    }

    async fn count(connection: &mut sqlx::PgConnection, ids: &[String]) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM program_applicant_reviews WHERE source_id = ANY($1)",
        )
        .bind(ids)
        .fetch_one(connection)
        .await
        .unwrap()
    }

    /// Curator file format (2026-10-15): extra `title`, evidence quoted from
    /// `plcyNm`, one with a trailing space.
    #[tokio::test]
    async fn imports_curator_lines_and_rejects_non_quotes_all_or_nothing() {
        let mut db = connection().await;
        let mut outer = db.begin().await.unwrap();
        let org = format!("t{}", uuid::Uuid::new_v4().simple());
        let indirect = format!("t{}", uuid::Uuid::new_v4().simple());
        for (source_id, title, method) in [
            (&org, "청년정책 전달체계 개선", ""),
            (
                &indirect,
                "반도체 특성화대학 지원",
                "지원대학 4곳 대상(개인 신청 불가)",
            ),
        ] {
            sqlx::query("INSERT INTO programs(program_type,source_type,source_id,title,program_status,raw_payload) VALUES('policy','youth_center',$1,$2,'active',$3)")
                .bind(source_id).bind(title)
                .bind(json!({"plcyNm": title, "plcyAplyMthdCn": method, "zipCd": "11110"}))
                .execute(&mut *outer).await.unwrap();
        }
        // The hash of the text the curators read (review M1).
        let read = |title: &str, method: &str| {
            applicant_review::text_hash(
                "youth_center",
                &json!({"plcyNm": title, "plcyAplyMthdCn": method}),
            )
            .unwrap()
        };
        let org_hash = read("청년정책 전달체계 개선", "");
        let indirect_hash = read(
            "반도체 특성화대학 지원",
            "지원대학 4곳 대상(개인 신청 불가)",
        );
        let good = [
            json!({"source_type":"youth_center","source_id":org,"evidence":"청년정책 전달체계 개선 ","reviewer":"curator:A+B@2026-10-15","title":"청년정책 전달체계 개선","class":"organisation","text_hash":org_hash}),
            json!({"source_type":"youth_center","source_id":indirect,"evidence":"지원대학 4곳 대상(개인 신청 불가)","reviewer":"curator:A+B@2026-10-15","title":"반도체 특성화대학 지원","class":"indirect","text_hash":indirect_hash}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        let bad = format!(
            "{good}\n{}\n{}\nnot json\n{}\n{}",
            json!({"source_type":"youth_center","source_id":org,"evidence":"기관만 신청하는 사업","reviewer":"curator:A","class":"organisation","text_hash":org_hash}),
            json!({"source_type":"youth_center","source_id":"missing-id","evidence":"청년정책 전달체계 개선","reviewer":"curator:A","class":"organisation","text_hash":org_hash}),
            json!({"source_type":"gov_benefits","source_id":"x","evidence":"청년정책 전달체계 개선","reviewer":"curator:A","class":"business","text_hash":org_hash}),
            // Review L3: an over-long key is a counted rejection, not a
            // database error that aborts the run.
            json!({"source_type":"youth_center","source_id":"9".repeat(201),"evidence":"청년정책 전달체계 개선","reviewer":"curator:A","class":"business","text_hash":org_hash}),
        );
        let summary = import_on(&mut outer, &bad, "import:test", false)
            .await
            .unwrap();
        assert!(!summary.applied);
        assert_eq!(summary.lines, 7);
        assert_eq!(summary.rejected, 5, "{summary:?}");
        assert_eq!(summary.rejected_by_reason["duplicate"], 1);
        assert_eq!(summary.rejected_by_reason["program_not_found"], 1);
        assert_eq!(summary.rejected_by_reason["invalid_json"], 1);
        assert_eq!(summary.rejected_by_reason["unsupported_source"], 1);
        assert_eq!(summary.rejected_by_reason["invalid_source"], 1);
        let ids = [org.clone(), indirect.clone()];
        assert_eq!(
            count(&mut outer, &ids).await,
            0,
            "nothing written on rejection"
        );

        // A non-quote alone is rejected too.
        let paraphrase = json!({"source_type":"youth_center","source_id":org,"evidence":"기관만 신청하는 사업","reviewer":"curator:A","class":"organisation","text_hash":org_hash}).to_string();
        let summary = import_on(&mut outer, &paraphrase, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.rejected_by_reason["evidence_not_found"], 1);

        // Review M1: a line must name the text it was reviewed on, and the
        // quote surviving a text change is not enough.
        let unhashed = json!({"source_type":"youth_center","source_id":org,"evidence":"청년정책 전달체계 개선","reviewer":"curator:A","class":"organisation"}).to_string();
        let summary = import_on(&mut outer, &unhashed, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.rejected_by_reason["missing_text_hash"], 1);
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"addAplyQlfcCndCn\":\"청년 개인도 직접 신청 가능\"}' WHERE source_type='youth_center' AND source_id=$1")
            .bind(&org).execute(&mut *outer).await.unwrap();
        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.rejected_by_reason["text_changed"], 1, "{summary:?}");
        assert!(!summary.applied);
        sqlx::query("UPDATE programs SET raw_payload = raw_payload - 'addAplyQlfcCndCn' WHERE source_type='youth_center' AND source_id=$1")
            .bind(&org).execute(&mut *outer).await.unwrap();

        let summary = import_on(&mut outer, &good, "import:test", true)
            .await
            .unwrap();
        assert_eq!(
            (summary.inserted, summary.rejected, summary.applied),
            (2, 0, false)
        );
        assert_eq!(count(&mut outer, &ids).await, 0, "dry run writes nothing");

        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!((summary.inserted, summary.applied), (2, true));
        assert_eq!(summary.accepted_by_class["indirect"], 1);
        let (evidence, hash): (String, String) = sqlx::query_as(
            "SELECT r.evidence, r.text_hash FROM program_applicant_reviews r WHERE source_id = $1",
        )
        .bind(&org)
        .fetch_one(&mut *outer)
        .await
        .unwrap();
        assert_eq!(evidence, "청년정책 전달체계 개선");
        assert_eq!(
            Some(hash),
            applicant_review::text_hash(
                "youth_center",
                &json!({"plcyNm": "청년정책 전달체계 개선", "plcyAplyMthdCn": ""})
            )
        );
        let summary = import_on(&mut outer, &good, "import:test", false)
            .await
            .unwrap();
        assert_eq!(summary.unchanged, 2, "re-import is idempotent");
        let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM program_applicant_review_log WHERE source_id = ANY($1) AND actor = 'import:test'")
            .bind(vec![org.clone(), indirect.clone()]).fetch_one(&mut *outer).await.unwrap();
        assert_eq!(logged, 2);

        // Security L1: the log is append-only in the database itself.
        for statement in [
            "UPDATE program_applicant_review_log SET actor = 'x' WHERE source_id = $1",
            "DELETE FROM program_applicant_review_log WHERE source_id = $1",
        ] {
            let mut savepoint = outer.begin().await.unwrap();
            let error = sqlx::query(statement)
                .bind(&org)
                .execute(&mut *savepoint)
                .await
                .expect_err(statement);
            assert!(error.to_string().contains("append-only"), "{error}");
            savepoint.rollback().await.unwrap();
        }
        outer.rollback().await.unwrap();
    }

    /// Security L2: the actor names who ran the import, not only the file.
    #[test]
    fn import_actor_names_the_operator() {
        assert_eq!(
            import_actor(Some("minseong"), Some("appuser".into()), "reviews.jsonl").unwrap(),
            "import:minseong:reviews.jsonl"
        );
        assert_eq!(
            import_actor(None, Some("opc".into()), "stdin").unwrap(),
            "import:opc:stdin"
        );
        assert!(import_actor(None, None, "stdin").is_err());
        assert!(import_actor(Some(" "), None, "stdin").is_err());
        assert!(import_actor(Some("a\nb"), None, "stdin").is_err());
        assert!(import_actor(Some(&"a".repeat(101)), None, "stdin").is_err());
    }
}
