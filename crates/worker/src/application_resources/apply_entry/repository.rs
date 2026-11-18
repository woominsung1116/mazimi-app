//! Storage of the apply-link job: `program_application_resources` rows of
//! kind `application` with `reader` set, their review evidence, and one
//! `program_apply_link_scans` row per programme. Nothing is deleted: a
//! replaced or failing link is marked `rejected` with its reason.
use super::{Failed, Outcome, Passed, Programme};
use mazimi_core::{
    application_resources::{fingerprint, AUTOMATED_REVIEWER},
    currently_available_sql,
    models::Program,
};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

/// A verified link is re-checked this often (its window is 7 days).
pub const RECHECK_SECS: f64 = 3.0 * 86_400.0;
/// Two hard failures in a row unpublish a verified link.
pub const HARD_FAILURES_TO_REJECT: i32 = 2;
const DAY: f64 = 86_400.0;

/// Every input a reader uses, hashed in SQL (the single definition): a
/// change re-derives the programme at the next run.
pub const INPUT_HASH_SQL: &str = "encode(sha256(convert_to(concat_ws(chr(31), 'apply-links-v1',
    p.source_type, p.official_url, p.admin_official_url, p.application_method,
    p.raw_payload->>'aplyUrlAddr', p.raw_payload->>'plcyNo',
    CASE WHEN p.source_type='gov_benefits' THEN s.raw_payload->'detail'->>'온라인신청사이트URL' END), 'UTF8')), 'hex')";

#[derive(FromRow)]
struct DueRow {
    id: Uuid,
    detail: Option<Value>,
    admin_set: bool,
    input_hash: String,
}

/// Current programmes whose scan is missing, due, or whose inputs changed:
/// confirmations of pending links first, then re-checks of verified ones,
/// then programmes never scanned, then the rest.
pub async fn due(pool: &PgPool, limit: i64) -> Result<Vec<Programme>, sqlx::Error> {
    let rows: Vec<DueRow> = sqlx::query_as(&format!(
        "WITH inputs AS (
           SELECT p.id, CASE WHEN p.source_type='gov_benefits' THEN s.raw_payload->'detail' END AS detail,
                  COALESCE(p.admin_official_url = p.official_url, false) AS admin_set,
                  {INPUT_HASH_SQL} AS input_hash
           FROM programs p
           LEFT JOIN source_snapshots s ON s.source_name=p.source_type AND s.source_id=p.source_id
           WHERE {available} AND p.official_url IS NOT NULL)
         SELECT i.id, i.detail, i.admin_set, i.input_hash
         FROM inputs i LEFT JOIN program_apply_link_scans sc ON sc.program_id=i.id
         WHERE sc.program_id IS NULL OR sc.next_scan_at <= now() OR sc.input_hash <> i.input_hash
         ORDER BY CASE WHEN sc.outcome='pending' THEN 0 WHEN sc.outcome='verified' THEN 1
                       WHEN sc.program_id IS NULL THEN 2 ELSE 3 END,
                  sc.next_scan_at NULLS FIRST, i.id
         LIMIT $1",
        available = currently_available_sql("p.", "now()"),
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let programs: Vec<Program> = sqlx::query_as("SELECT * FROM programs WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let program = programs.iter().find(|p| p.id == row.id)?.clone();
            Some(Programme {
                program,
                detail: row.detail.filter(Value::is_object),
                admin_official_url: row.admin_set,
                input_hash: row.input_hash,
            })
        })
        .collect())
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Recorded {
    pub inserted: usize,
    pub verified: usize,
    pub renewed: usize,
    pub restarted: usize,
    pub rejected: usize,
    pub demoted: usize,
    pub superseded: usize,
    pub human_kept: usize,
}

impl Recorded {
    pub fn add(&mut self, o: &Recorded) {
        self.inserted += o.inserted;
        self.verified += o.verified;
        self.renewed += o.renewed;
        self.restarted += o.restarted;
        self.rejected += o.rejected;
        self.demoted += o.demoted;
        self.superseded += o.superseded;
        self.human_kept += o.human_kept;
    }
}

#[derive(FromRow, Debug)]
struct Row {
    id: Uuid,
    status: String,
    reviewed_by: Option<Uuid>,
    reader: Option<String>,
    content_hash: Option<String>,
    source_hash: Option<String>,
    check_status: Option<String>,
    confirmed_due: bool,
    consecutive_gone: i32,
}

impl Row {
    /// A person decided this row (approved or rejected it).
    fn human(&self) -> bool {
        self.reviewed_by.is_some_and(|r| r != AUTOMATED_REVIEWER)
    }
    /// The job may change it: its own row, or a seeded candidate nobody
    /// reviewed.
    fn ours(&self) -> bool {
        !self.human() && (self.reader.is_some() || self.status == "candidate")
    }
}

async fn row(
    tx: &mut Transaction<'_, Postgres>,
    program: Uuid,
    url: &str,
    confirm_secs: f64,
) -> Result<Option<Row>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, status, reviewed_by, reader, content_hash, source_hash, check_status,
                COALESCE(checked_at <= now() - make_interval(secs => $3), false) AS confirmed_due,
                consecutive_gone
         FROM program_application_resources
         WHERE program_id=$1 AND kind='application' AND url=$2 FOR UPDATE",
    )
    .bind(program)
    .bind(url)
    .bind(confirm_secs)
    .fetch_optional(&mut **tx)
    .await
}

async fn review(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    decision: &str,
    evidence: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence)
         SELECT id,$2,$3,revision,$4 FROM program_application_resources WHERE id=$1",
    )
    .bind(id)
    .bind(AUTOMATED_REVIEWER)
    .bind(decision)
    .bind(evidence)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Marks the job's other live rows of a programme rejected (the official
/// page no longer points there). `keep` is the current target, if any.
async fn supersede(
    tx: &mut Transaction<'_, Postgres>,
    program: Uuid,
    keep: Option<&str>,
    reason: &str,
) -> Result<usize, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE program_application_resources SET status='rejected', valid_until=NULL, reviewed_by=NULL,
                review_note=NULL, check_error=$3, revision=revision+1, updated_at=now()
         WHERE program_id=$1 AND kind='application' AND reader IS NOT NULL
           AND status IN ('candidate','verified','unavailable')
           AND (reviewed_by IS NULL OR reviewed_by=$4)
           AND ($2::text IS NULL OR url <> $2)",
    )
    .bind(program)
    .bind(keep)
    .bind(format!("auto_rejected:{reason}"))
    .bind(AUTOMATED_REVIEWER)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() as usize)
}

/// A human-verified application of this programme: the job leaves the
/// programme to that review.
async fn human_verified(
    tx: &mut Transaction<'_, Postgres>,
    program: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM program_application_resources
         WHERE program_id=$1 AND kind='application' AND status='verified'
           AND reviewed_by IS NOT NULL AND reviewed_by <> $2 LIMIT 1",
    )
    .bind(program)
    .bind(AUTOMATED_REVIEWER)
    .fetch_optional(&mut **tx)
    .await
}

fn note(passed: &Passed) -> String {
    let step = match passed.step {
        "application_form" => "신청서 입력 화면",
        "login_gate" => "기관 로그인 후 이어지는 신청 화면",
        _ => "로그인 화면이 먼저 나오는 신청 화면",
    };
    format!(
        "자동 확인({}): 공식 페이지의 신청 버튼이 가리키는 곳을 두 번 열어 {}임을 확인했어요.",
        passed.reader.as_str(),
        step
    )
}

fn evidence(passed: &Passed) -> Value {
    json!({
        "reader": passed.reader.as_str(), "url": passed.url, "source_url": passed.source_url,
        "final_url": passed.final_url, "step": passed.step, "access": passed.access,
        "depth": passed.depth, "why": passed.why, "source_hash": passed.source_hash,
        "content_hash": passed.content_hash, "checks": 2
    })
}

fn check_status_of(code: &str) -> &'static str {
    if code.starts_with("http_") {
        "unreachable"
    } else if code == "not_a_page" {
        "unsupported"
    } else {
        "blocked"
    }
}

/// What the scan row says and when the programme is scanned again.
struct Scan<'a> {
    outcome: &'a str,
    reason: Option<String>,
    resource: Option<Uuid>,
    next_secs: f64,
}

async fn upsert_scan(
    tx: &mut Transaction<'_, Postgres>,
    p: &Programme,
    reader: Option<&str>,
    scan: Scan<'_>,
) -> Result<(), sqlx::Error> {
    let reason = scan.reason.map(|r| r.chars().take(80).collect::<String>());
    sqlx::query(
        "INSERT INTO program_apply_link_scans(program_id,outcome,reader,reason,input_hash,resource_id,scanned_at,next_scan_at,
           consecutive_unreachable)
         VALUES($1,$2,$3,$4,$5,$6,now(),now()+make_interval(secs => $7),CASE WHEN $2='unreachable' THEN 1 ELSE 0 END)
         ON CONFLICT(program_id) DO UPDATE SET outcome=excluded.outcome, reader=excluded.reader,
           reason=excluded.reason, input_hash=excluded.input_hash, resource_id=excluded.resource_id,
           scanned_at=now(), next_scan_at=excluded.next_scan_at,
           consecutive_unreachable=CASE WHEN excluded.outcome='unreachable'
             THEN CASE WHEN program_apply_link_scans.outcome='unreachable'
                       THEN program_apply_link_scans.consecutive_unreachable+1 ELSE 1 END
             ELSE 0 END",
    )
    .bind(p.program.id)
    .bind(scan.outcome)
    .bind(reader)
    .bind(reason)
    .bind(&p.input_hash)
    .bind(scan.resource)
    .bind(scan.next_secs)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Stores one programme's outcome.
pub async fn record(
    pool: &PgPool,
    p: &Programme,
    outcome: &Outcome,
    confirm_after: chrono::Duration,
    counts: &mut Recorded,
) -> anyhow::Result<()> {
    let confirm = confirm_after.num_seconds().max(0) as f64;
    let mut tx = pool.begin().await?;
    let program = p.program.id;
    if let Some(id) = human_verified(&mut tx, program).await? {
        counts.human_kept += 1;
        upsert_scan(
            &mut tx,
            p,
            None,
            Scan {
                outcome: "verified",
                reason: Some("human_reviewed".into()),
                resource: Some(id),
                next_secs: 14.0 * DAY,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    match outcome {
        Outcome::Offline { reason } => {
            counts.superseded += supersede(&mut tx, program, None, reason).await?;
            let scan = Scan {
                outcome: "offline",
                reason: Some((*reason).into()),
                resource: None,
                next_secs: 30.0 * DAY,
            };
            upsert_scan(&mut tx, p, None, scan).await?;
        }
        Outcome::NotFound { reader, reason } => {
            counts.superseded += supersede(&mut tx, program, None, reason).await?;
            let scan = Scan {
                outcome: "not_found",
                reason: Some(reason.clone()),
                resource: None,
                next_secs: 14.0 * DAY,
            };
            upsert_scan(&mut tx, p, Some(reader.as_str()), scan).await?;
        }
        Outcome::Ambiguous { reader, .. } => {
            // A link verified before stays until its window ends unless a
            // later check confirms it again.
            let scan = Scan {
                outcome: "ambiguous",
                reason: Some("tied_elements".into()),
                resource: None,
                next_secs: 14.0 * DAY,
            };
            upsert_scan(&mut tx, p, Some(reader.as_str()), scan).await?;
        }
        Outcome::Unreachable { reader, code } => {
            sqlx::query(
                "UPDATE program_application_resources SET check_status=$2, last_failure_code=$3,
                        consecutive_failures=consecutive_failures+1,
                        next_check_at=now()+interval '1 day', updated_at=now()
                 WHERE program_id=$1 AND kind='application' AND reader IS NOT NULL
                   AND status IN ('candidate','verified') AND (reviewed_by IS NULL OR reviewed_by=$4)",
            )
            .bind(program)
            .bind(if code == "robots" { "blocked" } else { "unreachable" })
            .bind(code.chars().take(100).collect::<String>())
            .bind(AUTOMATED_REVIEWER)
            .execute(&mut *tx)
            .await?;
            let previous: i32 = sqlx::query_scalar(
                "SELECT consecutive_unreachable FROM program_apply_link_scans
                 WHERE program_id=$1 AND outcome='unreachable' FOR UPDATE",
            )
            .bind(program)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);
            let scan = Scan {
                outcome: "unreachable",
                reason: Some(code.clone()),
                resource: None,
                next_secs: super::fetch::soft_retry(previous).as_secs_f64(),
            };
            upsert_scan(&mut tx, p, Some(reader.as_str()), scan).await?;
        }
        Outcome::Passed(passed) => {
            let scan = passed_row(&mut tx, p, passed, confirm, counts).await?;
            counts.superseded +=
                supersede(&mut tx, program, Some(&passed.url), "superseded").await?;
            upsert_scan(&mut tx, p, Some(passed.reader.as_str()), scan).await?;
        }
        Outcome::Failed(failed) => {
            let scan = failed_row(&mut tx, p, failed, counts).await?;
            counts.superseded +=
                supersede(&mut tx, program, Some(&failed.url), "superseded").await?;
            upsert_scan(&mut tx, p, Some(failed.reader.as_str()), scan).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn passed_row<'a>(
    tx: &mut Transaction<'_, Postgres>,
    p: &Programme,
    passed: &Passed,
    confirm: f64,
    counts: &mut Recorded,
) -> Result<Scan<'a>, sqlx::Error> {
    let fp = fingerprint(&p.program);
    let existing = row(tx, p.program.id, &passed.url, confirm).await?;
    let pending = |id| Scan {
        outcome: "pending",
        reason: Some(passed.step.into()),
        resource: Some(id),
        next_secs: confirm.max(60.0),
    };
    let Some(existing) = existing else {
        let id: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,status,access,
               program_fingerprint,discovery_depth,content_hash,source_hash,checked_at,check_status,next_check_at,reader)
             VALUES($1,'application',$2,$3,$4,'candidate',$5,$6,$7,$8,$9,now(),'ok',now()+make_interval(secs => $10),$11)
             ON CONFLICT (program_id,kind,url) DO NOTHING RETURNING id",
        )
        .bind(p.program.id)
        .bind(&passed.url)
        .bind(&passed.source_url)
        .bind(&passed.label)
        .bind(passed.access)
        .bind(&fp)
        .bind(passed.depth)
        .bind(&passed.content_hash)
        .bind(&passed.source_hash)
        .bind(confirm.max(60.0))
        .bind(passed.reader.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        counts.inserted += usize::from(id.is_some());
        return Ok(match id {
            Some(id) => pending(id),
            None => Scan {
                outcome: "pending",
                reason: Some("insert_race".into()),
                resource: None,
                next_secs: confirm.max(60.0),
            },
        });
    };
    if !existing.ours() {
        counts.human_kept += 1;
        // Only a person's decision is called one; a row another job
        // rejected is just not this job's to change.
        let human = existing.human();
        return Ok(Scan {
            outcome: "rejected",
            reason: Some(if human { "human_rejected" } else { "not_ours" }.into()),
            resource: Some(existing.id),
            next_secs: 14.0 * DAY,
        });
    }
    let same = existing.reader.is_some()
        && existing.content_hash.as_deref() == Some(passed.content_hash.as_str())
        && existing.source_hash.as_deref() == Some(passed.source_hash.as_str())
        && existing.check_status.as_deref() == Some("ok");
    let confirmable = same
        && match existing.status.as_str() {
            "verified" => true,
            "candidate" => existing.confirmed_due,
            _ => false,
        };
    if confirmable {
        sqlx::query(
            "UPDATE program_application_resources SET status='verified', verified_at=now(),
               valid_until=now()+interval '7 days', reviewed_by=$2, review_note=$3, access=$4, label=$5,
               source_url=$6, discovery_depth=$7, program_fingerprint=$8, checked_at=now(), check_status='ok',
               check_error=NULL, last_failure_code=NULL, consecutive_failures=0, consecutive_gone=0,
               next_check_at=now()+make_interval(secs => $9), revision=revision+1, updated_at=now()
             WHERE id=$1",
        )
        .bind(existing.id)
        .bind(AUTOMATED_REVIEWER)
        .bind(note(passed))
        .bind(passed.access)
        .bind(&passed.label)
        .bind(&passed.source_url)
        .bind(passed.depth)
        .bind(&fp)
        .bind(RECHECK_SECS)
        .execute(&mut **tx)
        .await?;
        if existing.status == "verified" {
            counts.renewed += 1;
        } else {
            counts.verified += 1;
            review(tx, existing.id, "verified", evidence(passed)).await?;
        }
        return Ok(Scan {
            outcome: "verified",
            reason: Some(passed.step.into()),
            resource: Some(existing.id),
            next_secs: RECHECK_SECS,
        });
    }
    if same && existing.status == "candidate" {
        // Passed again before the confirmation spacing: keep the first
        // check's time so the spacing is measured from it.
        return Ok(pending(existing.id));
    }
    // New, changed, or back after a rejection: a candidate again, timed
    // from this check.
    sqlx::query(
        "UPDATE program_application_resources SET status='candidate', valid_until=NULL, reviewed_by=NULL,
           review_note=NULL, access=$2, label=$3, source_url=$4, discovery_depth=$5, program_fingerprint=$6,
           content_hash=$7, source_hash=$8, reader=$9, checked_at=now(), check_status='ok', check_error=NULL,
           last_failure_code=NULL, consecutive_failures=0, consecutive_gone=0,
           next_check_at=now()+make_interval(secs => $10), revision=revision+1, updated_at=now()
         WHERE id=$1",
    )
    .bind(existing.id)
    .bind(passed.access)
    .bind(&passed.label)
    .bind(&passed.source_url)
    .bind(passed.depth)
    .bind(&fp)
    .bind(&passed.content_hash)
    .bind(&passed.source_hash)
    .bind(passed.reader.as_str())
    .bind(confirm.max(60.0))
    .execute(&mut **tx)
    .await?;
    counts.restarted += 1;
    Ok(pending(existing.id))
}

async fn failed_row<'a>(
    tx: &mut Transaction<'_, Postgres>,
    p: &Programme,
    failed: &Failed,
    counts: &mut Recorded,
) -> Result<Scan<'a>, sqlx::Error> {
    let rejected = |id: Option<Uuid>| Scan {
        outcome: "rejected",
        reason: Some(failed.code.clone()),
        resource: id,
        next_secs: 14.0 * DAY,
    };
    let code = failed.code.chars().take(80).collect::<String>();
    let Some(existing) = row(tx, p.program.id, &failed.url, 0.0).await? else {
        let id: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,status,access,
               program_fingerprint,discovery_depth,checked_at,check_status,check_error,last_failure_code,
               consecutive_gone,next_check_at,reader)
             VALUES($1,'application',$2,$3,$4,'rejected','unknown',$5,$6,now(),$7,$8,$9,1,now()+interval '14 days',$10)
             ON CONFLICT (program_id,kind,url) DO NOTHING RETURNING id",
        )
        .bind(p.program.id)
        .bind(&failed.url)
        .bind(&failed.source_url)
        .bind(&failed.label)
        .bind(fingerprint(&p.program))
        .bind(failed.depth)
        .bind(check_status_of(&code))
        .bind(format!("auto_rejected:{code}"))
        .bind(&code)
        .bind(failed.reader.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        counts.rejected += usize::from(id.is_some());
        return Ok(rejected(id));
    };
    if !existing.ours() {
        counts.human_kept += 1;
        return Ok(rejected(Some(existing.id)));
    }
    let gone = existing.consecutive_gone + 1;
    if existing.status == "verified" && gone < HARD_FAILURES_TO_REJECT {
        // One failure: users keep the link until the next check decides.
        sqlx::query(
            "UPDATE program_application_resources SET check_status=$2, last_failure_code=$3,
               consecutive_gone=consecutive_gone+1, checked_at=now(), next_check_at=now()+interval '1 day',
               updated_at=now() WHERE id=$1",
        )
        .bind(existing.id)
        .bind(check_status_of(&code))
        .bind(&code)
        .execute(&mut **tx)
        .await?;
        return Ok(Scan {
            outcome: "verified",
            reason: Some(format!("failing:{code}")),
            resource: Some(existing.id),
            next_secs: DAY,
        });
    }
    let was_verified = existing.status == "verified";
    sqlx::query(
        "UPDATE program_application_resources SET status='rejected', valid_until=NULL, reviewed_by=NULL,
           review_note=NULL, check_status=$2, last_failure_code=$3, check_error=$4,
           consecutive_gone=consecutive_gone+1, checked_at=now(), reader=COALESCE(reader,$5),
           next_check_at=now()+interval '14 days', revision=revision+1, updated_at=now() WHERE id=$1",
    )
    .bind(existing.id)
    .bind(check_status_of(&code))
    .bind(&code)
    .bind(format!("auto_rejected:{code}"))
    .bind(failed.reader.as_str())
    .execute(&mut **tx)
    .await?;
    if was_verified {
        counts.demoted += 1;
        review(
            tx,
            existing.id,
            "rejected",
            json!({"reader": failed.reader.as_str(), "url": failed.url, "code": code,
                   "consecutive_failures": gone}),
        )
        .await?;
    } else if existing.status != "rejected" {
        counts.rejected += 1;
    }
    Ok(rejected(Some(existing.id)))
}
