use super::{
    discovery, fetch, may_grace_renew, may_renew, noise_reason, Failure, NoiseRow, Observation,
    GONE_DEMOTION_THRESHOLD, GRACE_HOURS,
};
use chrono::{DateTime, Utc};
use mazimi_core::{
    application_resources::{
        fingerprint, publication_host, Resource, APPROVED_DOMAIN_SQL, MAX_RESOURCES,
        REDIRECTED_OFF_SITE,
    },
    currently_available_sql,
    models::Program,
};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Inserts a discovered candidate. The cap counts live rows only: rejected
/// rows are history and must not block new candidates. A concurrent insert
/// of the same (program, kind, url) is a no-op, never an error.
async fn insert(
    db: &mut PgConnection,
    p: &Program,
    c: &discovery::Candidate,
    source: &str,
    depth: i32,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,discovery_depth)
        SELECT $1,$2,$3,$4,$5,$6,$7 WHERE (SELECT count(*) FROM program_application_resources WHERE program_id=$1 AND status<>'rejected')<$8
        ON CONFLICT(program_id,kind,url) DO NOTHING")
        .bind(p.id).bind(c.kind).bind(&c.url).bind(source).bind(&c.label).bind(fingerprint(p)).bind(depth).bind(MAX_RESOURCES).execute(db).await?.rows_affected())
}
/// Queues the program's own URLs as candidates, but only those inside an
/// approved scope: a candidate nobody may fetch would only spend the
/// per-run check budget on an `approval_required` failure. A program is
/// rescanned after seven days, so a newly approved host is picked up then.
pub async fn seed(pool: &PgPool, limit: i64) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let scopes = fetch::approved_scopes(&mut *tx, None).await?;
    let programs = sqlx::query_as::<_, Program>(&format!(
        "SELECT * FROM programs WHERE {available}
         AND (application_resource_scan_at IS NULL OR application_resource_scan_at < now() - interval '7 days')
         ORDER BY application_resource_scan_at NULLS FIRST, id LIMIT $1 FOR UPDATE SKIP LOCKED",
        available = currently_available_sql("", "now()")
    ))
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    let mut count = 0;
    for p in programs {
        for seed in discovery::seeds(&p) {
            if fetch::scope_for(&scopes, &seed.url).is_some() {
                count += insert(&mut tx, &p, &seed, &seed.url, 0).await? as usize;
            }
        }
        sqlx::query("UPDATE programs SET application_resource_scan_at=now() WHERE id=$1")
            .bind(p.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(count)
}

/// 복지로 programmes of one source whose detail API was not asked in the
/// last seven days, currently available, oldest first.
pub async fn welfare_programs_due(
    pool: &PgPool,
    source_type: &str,
    limit: i64,
) -> Result<Vec<Program>, sqlx::Error> {
    if limit <= 0 {
        return Ok(vec![]);
    }
    sqlx::query_as::<_, Program>(&format!(
        "SELECT * FROM programs WHERE {available} AND source_type = $1
         AND (welfare_forms_scan_at IS NULL OR welfare_forms_scan_at < now() - interval '7 days')
         ORDER BY welfare_forms_scan_at NULLS FIRST, id LIMIT $2",
        available = currently_available_sql("", "now()")
    ))
    .bind(source_type)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn welfare_forms_scanned(pool: &PgPool, program: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE programs SET welfare_forms_scan_at = now() WHERE id = $1")
        .bind(program)
        .execute(pool)
        .await?;
    Ok(())
}

/// Queues files an official API lists for the programme as template
/// candidates on the programme's own level (depth 0); the file URL is its own
/// source, so no page is fetched for it.
pub async fn queue_files(
    pool: &PgPool,
    p: &Program,
    files: &[discovery::Candidate],
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut count = 0;
    for c in files {
        count += insert(&mut tx, p, c, &c.url, 0).await? as usize;
    }
    tx.commit().await?;
    Ok(count)
}

/// Publishes a download-only form ("공식 양식 받기"): the template candidate
/// becomes verified for a week (renewed by later unchanged probes like any
/// verified file), recorded with the rule's evidence. Only a candidate that
/// no reviewer has touched, still holding the bytes the rule read, is
/// published; anything else is left alone (`false`).
pub async fn publish_form(
    pool: &PgPool,
    row: &Resource,
    content_hash: &str,
    actor: Uuid,
    form: &super::form_check::Decision,
) -> Result<bool, sqlx::Error> {
    let title: String = form.title.chars().take(80).collect();
    let note = format!("자동 확인(다운로드 전용 양식): 공식 출처의 파일을 해시·형식으로 확인했고, 문서가 서식 '{title}'으로 시작합니다.");
    let mut tx = pool.begin().await?;
    let revision: Option<i64> = sqlx::query_scalar(
        "UPDATE program_application_resources SET status='verified', access='direct', verified_at=now(),
            valid_until=now()+interval '168 hours', reviewed_by=$2, review_note=$3, revision=revision+1, updated_at=now()
         WHERE id=$1 AND kind='template' AND status='candidate' AND reviewed_by IS NULL AND content_hash=$4
           AND check_status='ok' AND file_format IS NOT NULL AND byte_size IS NOT NULL AND checked_at IS NOT NULL
         RETURNING revision",
    )
    .bind(row.id)
    .bind(actor)
    .bind(&note)
    .bind(content_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(revision) = revision else {
        return Ok(false);
    };
    sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,'verified',$3,$4)")
        .bind(row.id)
        .bind(actor)
        .bind(revision)
        // `content_hash` is what a grace renewal compares (GRACE_WINDOW_SQL).
        .bind(serde_json::json!({"automatic": true, "rule": "download_form_v1", "tier": form.reason,
            "form_title": title, "sha256": content_hash, "content_hash": content_hash,
            "model_answer": form.model, "validity_hours": 168}))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// The catalogue the form programme match reads: every current programme's
/// title, provider, regions and district codes (`form_check::Vocabulary`).
pub async fn catalogue(pool: &PgPool) -> Result<super::form_check::Vocabulary, sqlx::Error> {
    type Row = (
        String,
        Option<String>,
        Option<Vec<String>>,
        Option<Vec<String>>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT title, provider_name, regions, region_districts FROM current_programs",
    )
    .fetch_all(pool)
    .await?;
    Ok(super::form_check::Vocabulary::from_catalogue(
        rows.iter().map(
            |(title, provider, regions, districts)| super::form_check::Programme {
                title,
                provider: provider.as_deref().unwrap_or(""),
                regions: regions.as_deref().unwrap_or(&[]),
                districts: districts.as_deref().unwrap_or(&[]),
            },
        ),
    ))
}

/// Undoes the download-only tier: rejects every template whose latest review
/// is this rule's own automatic `verified` (`evidence.rule =
/// download_form_v1`) under `actor`, the tier's own account, and records a
/// `rejected` review for each. A template a person reviewed since, or
/// verified by hand, is not touched. Returns the
/// resources rejected.
pub async fn undo_download_forms(pool: &PgPool, actor: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rejected: Vec<(Uuid, i64)> = sqlx::query_as(
        "WITH latest AS (
            SELECT DISTINCT ON (resource_id) resource_id, reviewer_id, decision, evidence
              FROM application_resource_reviews ORDER BY resource_id, created_at DESC, id DESC)
         UPDATE program_application_resources r SET status='rejected', valid_until=NULL, reviewed_by=$1,
                review_note='다운로드 전용 양식 자동 게시를 되돌렸습니다(download_form_v1).',
                revision=r.revision+1, updated_at=now()
           FROM latest l
          WHERE l.resource_id=r.id AND r.kind='template' AND r.status='verified'
            AND l.decision='verified' AND l.evidence->>'rule'='download_form_v1' AND l.reviewer_id=$1
          RETURNING r.id, r.revision",
    )
    .bind(actor)
    .fetch_all(&mut *tx)
    .await?;
    for (id, revision) in &rejected {
        sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,'rejected',$3,$4)")
            .bind(id)
            .bind(actor)
            .bind(revision)
            .bind(serde_json::json!({"automatic": true, "rule": "download_form_v1", "undo": true}))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rejected.into_iter().map(|(id, _)| id).collect())
}

/// Leases due rows for two hours. Verified templates go first: users see
/// them only while a re-check keeps renewing them, and an approval needs
/// a probe from the last 24 hours, so they must not queue behind the
/// candidate backlog (10-16: five runs passed without reaching them). The lease does not touch `revision`: the
/// revision is the reviewer's optimistic lock, and a background lease is not
/// a change to the reviewed evidence (an admin who opened the list before
/// the lease must still be able to approve). Admin writes bump the revision,
/// so `WHERE revision=$2` in the result writes still detects them.
///
/// Only rows whose URL is inside an approved scope are leased, so the
/// per-run budget goes to rows that can actually be fetched; the rest wait,
/// unchanged, until their host is approved. The host prefilter runs in SQL
/// and [`fetch::scope_for`] decides, the same check the fetch repeats.
pub async fn claim(pool: &PgPool, limit: i64) -> Result<Vec<Resource>, sqlx::Error> {
    let scopes = fetch::approved_scopes(pool, None).await?;
    if scopes.is_empty() || limit <= 0 {
        return Ok(vec![]);
    }
    // Only resources of programs a user can act on now are probed; a closed
    // or upcoming program is neither leased nor fetched.
    let due: Vec<(Uuid, String)> = sqlx::query_as(&format!(
        "SELECT r.id, r.url FROM program_application_resources r JOIN current_programs p ON p.id = r.program_id
        WHERE r.status <> 'rejected' AND r.next_check_at <= now() AND r.reader IS NULL AND {available}
          AND EXISTS (SELECT 1 FROM benefit_source_domains d WHERE {approved} AND {host})
        ORDER BY (r.kind='template' AND r.status='verified') DESC, r.next_check_at, r.id LIMIT {CLAIM_SCAN_LIMIT}",
        available = currently_available_sql("p.", "now()"),
        approved = fetch::APPROVED_SCOPE_SQL,
        host = fetch::scope_host_sql(URL_HOST_SQL),
    ))
    .fetch_all(pool)
    .await?;
    let (in_scope, outside): (Vec<_>, Vec<_>) = due
        .into_iter()
        .partition(|(_, url)| fetch::scope_for(&scopes, url).is_some());
    let ids: Vec<Uuid> = in_scope
        .into_iter()
        .map(|(id, _)| id)
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect();
    // A row on an approved host but outside its path prefixes (a discovered
    // link, a redirect target) is not fetchable either. Move it back a day
    // without recording a failure, so such rows cannot fill the scan window
    // and starve the fetchable ones behind them.
    let outside: Vec<Uuid> = outside.into_iter().map(|(id, _)| id).collect();
    if !outside.is_empty() {
        sqlx::query("UPDATE program_application_resources SET next_check_at = now() + interval '1 day' WHERE id = ANY($1) AND next_check_at <= now()")
            .bind(&outside)
            .execute(pool)
            .await?;
    }
    // Re-checking next_check_at makes a concurrent claim of the same row a
    // no-op: the second UPDATE sees the first lease and skips it.
    let mut leased: Vec<Resource> = sqlx::query_as(
        "UPDATE program_application_resources r SET next_check_at = now() + interval '2 hours'
        WHERE r.id = ANY($1) AND r.status <> 'rejected' AND r.next_check_at <= now() RETURNING r.*",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    // RETURNING has no order: hand rows out in claim priority, so a run that
    // stops at its time budget has already renewed the verified templates.
    leased.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
    Ok(leased)
}

/// The lowercase host of `r.url`, without a trailing dot.
const URL_HOST_SQL: &str =
    "rtrim(lower(substring(r.url from '^[A-Za-z][A-Za-z0-9+.-]*://([^/?#:@]+)')), '.')";

/// Due rows on approved hosts examined per claim. Path filtering happens in
/// Rust, so this bounds memory, not the lease.
const CLAIM_SCAN_LIMIT: i64 = 5000;

/// The program as it is now, if a user can still act on it
/// (`Program::is_currently_available`, the twin of the claim filter).
pub async fn available_program(pool: &PgPool, id: Uuid) -> Result<Option<Program>, sqlx::Error> {
    let program = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(program.filter(|p| p.is_currently_available(Utc::now())))
}

/// The domain slot is reserved beyond the wait cap (sibling Retry-After or a
/// busy domain). Nothing was observed, so only the schedule moves.
pub async fn defer(pool: &PgPool, row: &Resource, until: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE program_application_resources SET next_check_at=GREATEST($3, now()+interval '1 minute'),updated_at=now() WHERE id=$1 AND revision=$2")
        .bind(row.id).bind(row.revision).bind(until).execute(pool).await?;
    Ok(())
}

/// A login_required destination answered with its own sign-in redirect: the
/// healthy observation. Clears failure counters; never renews trust, because
/// a login page proves nothing about the form behind it.
pub async fn login_gate(pool: &PgPool, row: &Resource) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE program_application_resources SET check_status='ok',check_error=NULL,last_failure_code=NULL,consecutive_failures=0,consecutive_gone=0,
        checked_at=now(),next_check_at=now()+interval '6 hours',updated_at=now() WHERE id=$1 AND revision=$2 AND status<>'rejected'")
        .bind(row.id).bind(row.revision).execute(pool).await?;
    Ok(())
}

/// A redirect is an observation for the reviewer, never authority. The target
/// is queued as a notice (or template, for a file download), never as an
/// application: a login page or a moved homepage must not be offered as the
/// "real application screen" candidate.
pub async fn redirect_candidate(
    pool: &PgPool,
    row: &Resource,
    p: &Program,
    target: &str,
) -> Result<(), sqlx::Error> {
    if row.discovery_depth >= 2 || target == row.url {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let Some(current) =
        sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1 FOR UPDATE")
            .bind(p.id)
            .fetch_optional(&mut *tx)
            .await?
    else {
        return Ok(());
    };
    let revision:Option<i64>=sqlx::query_scalar("SELECT revision FROM program_application_resources WHERE id=$1 AND status<>'rejected' FOR UPDATE").bind(row.id).fetch_optional(&mut *tx).await?;
    if revision != Some(row.revision) || fingerprint(&current) != fingerprint(p) {
        return Ok(());
    }
    insert(
        &mut tx,
        p,
        &discovery::Candidate {
            kind: redirect_kind(&row.kind),
            url: target.into(),
            label: row.label.clone(),
        },
        &row.source_url,
        row.discovery_depth + 1,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
/// A notice or application URL that served a document: the same URL is
/// queued as a form candidate at the same depth (the bytes decide its format
/// again when it is probed as a template).
pub async fn file_candidate(pool: &PgPool, row: &Resource, p: &Program) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    insert(
        &mut tx,
        p,
        &discovery::Candidate {
            kind: "template",
            url: row.url.clone(),
            label: row.label.clone(),
        },
        &row.source_url,
        row.discovery_depth,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
pub(super) fn redirect_kind(kind: &str) -> &'static str {
    if kind == "template" {
        "template"
    } else {
        "notice"
    }
}

/// Records a failed probe on the observation columns only. The reviewer's
/// `status` is untouched, except that a verified row is demoted to
/// `unavailable` after [`GONE_DEMOTION_THRESHOLD`] consecutive 404/410.
/// The revision moves only on that demotion, so an admin who is looking at
/// the row is not handed a spurious conflict. Returns true on demotion.
/// An off-site redirect code ([`REDIRECTED_OFF_SITE`]) is sticky across
/// later failures (review M-1): a timeout after the redirect says nothing
/// about where the entry leads, so the row stays hidden until a clean
/// probe (`success`, `login_gate`) or a review clears it.
pub async fn failure(pool: &PgPool, row: &Resource, f: &Failure) -> Result<bool, sqlx::Error> {
    let demoted: Option<bool> = sqlx::query_scalar(
        "UPDATE program_application_resources r SET
            check_status=$3, check_error=$4,
            last_failure_code=CASE WHEN r.last_failure_code=$8 THEN r.last_failure_code ELSE $4 END,
            consecutive_failures=LEAST(r.consecutive_failures+1, 100000),
            consecutive_gone=CASE WHEN $5 THEN LEAST(r.consecutive_gone+1, 100000) ELSE 0 END,
            status=CASE WHEN r.status='verified' AND $5 AND r.consecutive_gone+1>=$7 THEN 'unavailable' ELSE r.status END,
            valid_until=CASE WHEN r.status='verified' AND $5 AND r.consecutive_gone+1>=$7 THEN NULL ELSE r.valid_until END,
            revision=r.revision+CASE WHEN r.status='verified' AND $5 AND r.consecutive_gone+1>=$7 THEN 1 ELSE 0 END,
            checked_at=now(), next_check_at=now()+make_interval(secs=>$6::double precision), updated_at=now()
         FROM program_application_resources old
         WHERE r.id=$1 AND old.id=r.id AND r.revision=$2 AND r.status<>'rejected'
         RETURNING old.status='verified' AND r.status='unavailable'",
    )
    .bind(row.id)
    .bind(row.revision)
    .bind(f.check_status)
    .bind(&f.code)
    .bind(f.gone)
    .bind(f.retry_after_secs as f64)
    .bind(GONE_DEMOTION_THRESHOLD)
    .bind(REDIRECTED_OFF_SITE)
    .fetch_optional(pool)
    .await?;
    Ok(demoted.unwrap_or(false))
}

/// The window a successful re-check renews a verified row for: the
/// validity the reviewer chose at the last approval (1–168 hours, from the
/// review record), else the row's current window, else 24 hours; never
/// more than 168 hours (the table CHECK allows 7 days). Before this, every
/// renewal shrank a 168-hour approval to one day.
const RENEWAL_WINDOW_SQL: &str = "LEAST(interval '168 hours', COALESCE(
    (SELECT make_interval(hours => LEAST(GREATEST((v.evidence->>'validity_hours')::int, 1), 168))
       FROM application_resource_reviews v
      WHERE v.resource_id = r.id AND v.decision = 'verified' AND (v.evidence->>'validity_hours') ~ '^[0-9]{1,4}$'
      ORDER BY v.created_at DESC LIMIT 1),
    NULLIF(GREATEST(r.valid_until - r.verified_at, interval '0'), interval '0'),
    interval '24 hours'))";

/// The window a grace renewal grants: only the validity the reviewer chose
/// in the latest verified review, and only when that review saw exactly the
/// re-fetched file (`$4`). No review record, no recorded hash, or a
/// different hash: NULL, so nothing renews and a reviewer must look again.
const GRACE_WINDOW_SQL: &str =
    "(SELECT make_interval(hours => LEAST(GREATEST((v.evidence->>'validity_hours')::int, 1), 168))
       FROM (SELECT evidence FROM application_resource_reviews
              WHERE resource_id = r.id AND decision = 'verified'
              ORDER BY created_at DESC LIMIT 1) v
      WHERE (v.evidence->>'validity_hours') ~ '^[0-9]{1,4}$'
        AND v.evidence->>'content_hash' = $4)";

/// What one noise sweep did (or, in a dry run, would do).
#[derive(Debug, Default)]
pub struct NoiseSweep {
    /// Candidate templates marked `rejected`, with the rule.
    pub rejected: Vec<(NoiseRow, &'static str)>,
    /// Earlier automatic rejections the current rules no longer support,
    /// put back to `candidate` and due now.
    pub restored: Vec<NoiseRow>,
}

/// The facts [`noise_reason`] needs, for candidate templates and for rows an
/// earlier sweep rejected automatically (never for a reviewer's decision).
/// Per-URL counts are computed once (one pass, indexed by
/// `application_resources_template_url`). A file shared by programmes
/// counts every template row, rejected ones included, so a sweep cannot
/// undo its own evidence. The titles of programmes whose window ended (or
/// whose source says closed) and that carry the same file are returned;
/// [`noise_reason`] counts only those about the same programme.
const NOISE_ROWS_SQL: &str = "WITH shared AS (
    SELECT url, count(DISTINCT program_id) AS programs FROM program_application_resources
     WHERE kind = 'template' GROUP BY url),
  ended AS (
    SELECT o.url, o.program_id, q.title FROM program_application_resources o JOIN programs q ON q.id = o.program_id
     WHERE o.kind = 'template' AND (q.program_status = 'closed'
       OR LEAST(q.benefit_valid_until, q.deadline_at, q.application_end_at) <= now()))
  SELECT r.id, r.revision, r.label, r.discovery_depth, p.title AS program_title,
    (SELECT s.label FROM program_application_resources s
      WHERE s.program_id = r.program_id AND s.url = r.source_url AND s.kind <> 'template'
      ORDER BY s.discovery_depth, s.id LIMIT 1) AS source_label,
    coalesce(sh.programs, 0) AS programs_sharing_url,
    ARRAY(SELECT DISTINCT e.title FROM ended e WHERE e.url = r.url AND e.program_id <> r.program_id ORDER BY e.title) AS ended_titles,
    r.status = 'rejected' AS auto_rejected
  FROM program_application_resources r JOIN programs p ON p.id = r.program_id
  LEFT JOIN shared sh ON sh.url = r.url
  WHERE r.kind = 'template' AND (r.status = 'candidate'
    OR (r.status = 'rejected' AND r.reviewed_by IS NULL AND starts_with(coalesce(r.check_error, ''), 'auto_rejected:')))
  ORDER BY r.id";

/// Candidate templates that are noise (see [`noise_reason`]) are marked
/// `rejected` with the reason; when `apply` is false nothing is written.
/// Rows are operational evidence, so nothing is deleted: a rejected row
/// stays as history and, through the (program, kind, url) key, keeps the
/// same file from being queued again. Reviewed rows are never touched.
/// The sweep is reversible on its own: an automatic rejection that the
/// current rules no longer support (a rule was corrected, or the evidence
/// changed) goes back to `candidate` and is probed on the next claim.
pub async fn reject_noise(pool: &PgPool, apply: bool) -> Result<NoiseSweep, sqlx::Error> {
    let rows: Vec<NoiseRow> = sqlx::query_as(NOISE_ROWS_SQL).fetch_all(pool).await?;
    let mut planned = NoiseSweep::default();
    for row in rows {
        match (noise_reason(&row), row.auto_rejected) {
            (Some(reason), false) => planned.rejected.push((row, reason)),
            (None, true) => planned.restored.push(row),
            _ => {}
        }
    }
    if !apply {
        return Ok(planned);
    }
    let mut done = NoiseSweep::default();
    for (row, reason) in planned.rejected {
        let updated = sqlx::query(
            "UPDATE program_application_resources SET status='rejected', valid_until=NULL,
                check_error=$3, review_note=$4, revision=revision+1, updated_at=now()
             WHERE id=$1 AND revision=$2 AND status='candidate' AND kind='template'",
        )
        .bind(row.id)
        .bind(row.revision)
        .bind(format!("auto_rejected:{reason}"))
        .bind(noise_note(reason))
        .execute(pool)
        .await?
        .rows_affected();
        if updated == 1 {
            done.rejected.push((row, reason));
        }
    }
    for row in planned.restored {
        let updated = sqlx::query(
            "UPDATE program_application_resources SET status='candidate', check_error=NULL, review_note=NULL,
                revision=revision+1, next_check_at=now(), updated_at=now()
             WHERE id=$1 AND revision=$2 AND status='rejected' AND reviewed_by IS NULL
               AND starts_with(coalesce(check_error, ''), 'auto_rejected:')",
        )
        .bind(row.id)
        .bind(row.revision)
        .execute(pool)
        .await?
        .rows_affected();
        if updated == 1 {
            done.restored.push(row);
        }
    }
    Ok(done)
}

fn noise_note(reason: &str) -> &'static str {
    match reason {
        "recruitment_document" => "Automatically rejected: a recruitment or HR document (identity statement, medical check, exam application, demand survey), not a benefit application form.",
        "shared_attachment" => "Automatically rejected: the same file is attached to three or more programmes, so it is a board-wide attachment, not this programme's form.",
        "earlier_round" => "Automatically rejected: the same file is attached to a programme that is no longer open, so it is an earlier round's form.",
        _ => "Automatically rejected: found on a linked page whose title shares no distinctive word with this programme.",
    }
}

/// A verified template at risk of expiring for users.
#[derive(Debug, sqlx::FromRow)]
pub struct AtRisk {
    pub id: Uuid,
    pub program_id: Uuid,
    pub verified_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub check_status: Option<String>,
    pub last_failure_code: Option<String>,
    pub next_check_at: DateTime<Utc>,
}

/// At risk: no successful re-check for `stale_hours` (`verified_at` moves
/// only on approval and on a successful renewal, so it is the last
/// success), or the window ends within `expiry_hours` (half the window for
/// one shorter than twice that, so a short approved window that is renewed
/// on schedule never alerts), or already ended, so
/// the warning comes before users lose the template. A `changed` file is
/// already hidden and reported, and a row `claim` would never lease (its
/// programme is not current and open) is not probed, so neither counts.
pub async fn at_risk_templates(
    pool: &PgPool,
    stale_hours: i64,
    expiry_hours: i64,
) -> Result<Vec<AtRisk>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT r.id, r.program_id, r.verified_at, r.valid_until, r.check_status, r.last_failure_code, r.next_check_at
           FROM program_application_resources r JOIN current_programs p ON p.id = r.program_id
          WHERE r.kind='template' AND r.status='verified' AND r.check_status IS DISTINCT FROM 'changed'
            AND (r.verified_at < now() - make_interval(hours => $1)
                 OR r.valid_until < now() + LEAST(make_interval(hours => $2), (r.valid_until - r.verified_at) / 2))
            AND {available}
          ORDER BY r.id",
        available = currently_available_sql("p.", "now()"),
    ))
    .bind(stale_hours as i32)
    .bind(expiry_hours as i32)
    .fetch_all(pool)
    .await
}

/// Open at-risk events: fingerprint and whether they already say "expired".
pub async fn open_at_risk_events(pool: &PgPool) -> Result<Vec<(String, bool)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT fingerprint, coalesce((details->>'expired')::boolean, false) FROM operational_events
          WHERE starts_with(fingerprint, 'application_resource:at_risk:') AND status <> 'resolved'",
    )
    .fetch_all(pool)
    .await
}

pub async fn event_exists(pool: &PgPool, fingerprint: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operational_events WHERE fingerprint=$1)")
        .bind(fingerprint)
        .fetch_one(pool)
        .await
}

pub async fn open_daily_summaries(pool: &PgPool, keep: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT fingerprint FROM operational_events
          WHERE starts_with(fingerprint, 'application_resources:daily:') AND fingerprint <> $1 AND status <> 'resolved'",
    )
    .bind(keep)
    .fetch_all(pool)
    .await
}

/// Last 24 hours (found, checked by outcome, rejected by reason) and the
/// current queue (verified usable/expired, unsupported), per kind. Counts
/// and reason codes only, never crawled text. `verified` counts only what
/// users can see: an unexpired verified row hidden by an off-site redirect
/// (M7) or an unvouched host (M6) is counted under `hidden` instead
/// (review L-4).
pub async fn daily_counts(pool: &PgPool) -> Result<serde_json::Value, sqlx::Error> {
    let mut counts = queue_counts(pool).await?;
    let hidden = hidden_verified(pool).await?;
    for kinds in hidden.values() {
        for (kind, n) in kinds {
            if let Some(verified) = counts["verified"][kind.as_str()].as_i64() {
                counts["verified"][kind.as_str()] = (verified - n).max(0).into();
            }
        }
    }
    if !hidden.is_empty() {
        counts["hidden"] = serde_json::to_value(&hidden).unwrap_or_default();
    }
    Ok(counts)
}

/// Hidden row counts by reason, then kind.
type Hidden = std::collections::BTreeMap<&'static str, std::collections::BTreeMap<String, i64>>;

#[derive(sqlx::FromRow)]
struct VerifiedHost {
    kind: String,
    url: String,
    official_url: Option<String>,
    admin_set: bool,
    last_failure_code: Option<String>,
}

/// Unexpired verified rows users do not see, by reason then kind.
async fn hidden_verified(pool: &PgPool) -> Result<Hidden, sqlx::Error> {
    let rows: Vec<VerifiedHost> = sqlx::query_as(
        "SELECT r.kind, r.url, p.official_url, COALESCE(p.admin_official_url = p.official_url, false) AS admin_set,
                r.last_failure_code
           FROM program_application_resources r JOIN programs p ON p.id = r.program_id
          WHERE r.status = 'verified' AND r.valid_until > now()",
    )
    .fetch_all(pool)
    .await?;
    let mut hidden = Hidden::new();
    for VerifiedHost {
        kind,
        url,
        official_url,
        admin_set,
        last_failure_code,
    } in rows
    {
        let reason = if last_failure_code.as_deref() == Some(REDIRECTED_OFF_SITE) {
            "redirect_off_site"
        } else {
            let vouched = match publication_host(&url, official_url.as_deref()) {
                None => false,
                Some(rule) if rule.official && !admin_set => true,
                Some(rule) => {
                    sqlx::query_scalar::<_, bool>(APPROVED_DOMAIN_SQL)
                        .bind(&rule.host)
                        .bind(rule.exact)
                        .fetch_one(pool)
                        .await?
                }
            };
            if vouched {
                continue;
            }
            "unapproved_host"
        };
        *hidden.entry(reason).or_default().entry(kind).or_default() += 1;
    }
    Ok(hidden)
}

async fn queue_counts(pool: &PgPool) -> Result<serde_json::Value, sqlx::Error> {
    sqlx::query_scalar(
        "WITH r AS (SELECT * FROM program_application_resources),
         counts AS (
           SELECT 'found_24h' AS metric, kind AS key, count(*) AS n FROM r WHERE created_at > now()-interval '24 hours' GROUP BY kind
           UNION ALL SELECT 'checked_24h', kind || ':' || coalesce(check_status,'none'), count(*) FROM r
             WHERE checked_at > now()-interval '24 hours' AND status <> 'rejected' GROUP BY 1,2
           UNION ALL SELECT 'rejected_24h', coalesce(check_error, 'reviewer'), count(*) FROM r
             WHERE status='rejected' AND updated_at > now()-interval '24 hours' GROUP BY 1,2
           UNION ALL SELECT 'rejected_total', coalesce(check_error, 'reviewer'), count(*) FROM r WHERE status='rejected' GROUP BY 1,2
           UNION ALL SELECT 'verified', kind, count(*) FROM r WHERE status='verified' AND valid_until > now() GROUP BY kind
           UNION ALL SELECT 'expired', kind, count(*) FROM r WHERE status='verified' AND valid_until <= now() GROUP BY kind
           UNION ALL SELECT 'unsupported', kind, count(*) FROM r WHERE check_status='unsupported' AND status <> 'rejected' GROUP BY kind
           UNION ALL SELECT 'status', kind || ':' || status, count(*) FROM r GROUP BY 1,2)
         SELECT coalesce(jsonb_object_agg(metric, keys), '{}'::jsonb)::json FROM (
           SELECT metric, jsonb_object_agg(key, n) AS keys FROM counts GROUP BY metric) m",
    )
    .fetch_one(pool)
    .await
}

/// What a successful probe did to the row.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub updated: bool,
    /// A verified row was observed to differ from its reviewed version for
    /// the first time (it is now `check_status='changed'`).
    pub newly_changed: bool,
    /// An expired template was renewed through the grace path.
    pub grace_renewed: bool,
}

/// Records a successful probe. A verified row is never demoted here: a
/// difference from the reviewed version sets the sticky
/// `check_status='changed'` (cleared only by a new admin review). A changed
/// template is hidden by `usable`; a changed HTML page keeps its reviewed
/// destination, so a page keeps the reviewed `program_fingerprint` and a
/// new intake round fails `Resource::usable` until someone reviews it. A
/// template takes the new round's fingerprint (review H1): the admin review
/// requires a probe for this round, and the sticky `changed` flag, set by
/// any fingerprint difference, keeps it hidden until that review.
/// The revision moves only when the reviewed evidence columns change.
pub async fn success(
    pool: &PgPool,
    row: &Resource,
    p: &Program,
    observation: Observation,
    changed: bool,
) -> Result<Recorded, sqlx::Error> {
    let now = Utc::now();
    let renew = may_renew(row, changed, now);
    let grace = !renew && may_grace_renew(row, &observation, changed, now);
    let mut tx = pool.begin().await?;
    // Match the program snapshot after network I/O, then optimistic-lock the
    // resource. An in-flight worker cannot overwrite a newer admin rejection.
    let Some(current) =
        sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1 FOR UPDATE")
            .bind(p.id)
            .fetch_optional(&mut *tx)
            .await?
    else {
        return Ok(Recorded::default());
    };
    if fingerprint(&current) != fingerprint(p) {
        return Ok(Recorded::default());
    }
    // `$10` (grace) is re-checked here against the row as it is now: still
    // expired by less than GRACE_HOURS, unchanged, and the stored hashes
    // equal to the observation; GRACE_WINDOW_SQL adds the review record.
    let grace_sql = format!(
        "($10 AND r.status='verified' AND r.kind='template' AND r.check_status IS DISTINCT FROM 'changed'
          AND r.valid_until <= now() AND r.valid_until > now() - make_interval(hours => {GRACE_HOURS})
          AND r.content_hash = $4 AND r.source_hash = $5 AND {GRACE_WINDOW_SQL} IS NOT NULL)"
    );
    let written: Option<(bool, bool)> = sqlx::query_as(&format!(
        "UPDATE program_application_resources r SET
            program_fingerprint=CASE WHEN r.status='verified' AND r.kind<>'template' THEN r.program_fingerprint ELSE $3 END,
            content_hash=$4, source_hash=$5, file_format=$6, byte_size=$7,
            check_status=CASE WHEN r.status='verified' AND ($8 OR r.check_status='changed'
                OR r.program_fingerprint IS DISTINCT FROM $3) THEN 'changed' ELSE 'ok' END,
            verified_at=CASE WHEN $9 OR {grace_sql} THEN now() ELSE r.verified_at END,
            valid_until=CASE WHEN $9 THEN now()+{RENEWAL_WINDOW_SQL}
                             WHEN {grace_sql} THEN now()+{GRACE_WINDOW_SQL}
                             ELSE r.valid_until END,
            revision=r.revision+CASE WHEN r.content_hash IS DISTINCT FROM $4 OR r.source_hash IS DISTINCT FROM $5
                OR r.file_format IS DISTINCT FROM $6 OR r.byte_size IS DISTINCT FROM $7
                OR (r.status='verified' AND r.kind='template' AND r.program_fingerprint IS DISTINCT FROM $3) THEN 1 ELSE 0 END,
            checked_at=now(), check_error=NULL, last_failure_code=NULL, consecutive_failures=0, consecutive_gone=0,
            next_check_at=now()+interval '6 hours', updated_at=now()
         FROM program_application_resources old
         WHERE r.id=$1 AND old.id=r.id AND r.revision=$2 AND r.status<>'rejected'
         RETURNING r.check_status='changed' AND old.check_status IS DISTINCT FROM 'changed',
                   NOT $9 AND r.valid_until IS DISTINCT FROM old.valid_until",
    ))
    .bind(row.id)
    .bind(row.revision)
    .bind(fingerprint(p))
    .bind(&observation.hash)
    .bind(&observation.source_hash)
    .bind(&observation.file_format)
    .bind(observation.size)
    .bind(changed)
    .bind(renew)
    .bind(grace)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((newly_changed, grace_renewed)) = written else {
        tx.commit().await?;
        return Ok(Recorded::default());
    };
    for c in observation.links {
        insert(&mut tx, p, &c, &row.url, row.discovery_depth + 1).await?;
    }
    tx.commit().await?;
    Ok(Recorded {
        updated: true,
        newly_changed,
        grace_renewed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_resources::fetch::FetchError;
    use uuid::Uuid;

    async fn pool() -> Option<PgPool> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping database-backed resource test");
            return None;
        };
        Some(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .unwrap(),
        )
    }
    async fn program(pool: &PgPool) -> Program {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url) VALUES($1,'policy','manual','resource-worker-fixture','active',true,'https://official.example.org/notice')")
            .bind(id).execute(pool).await.unwrap();
        sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }
    /// The download-only tier publishes only an untouched candidate that
    /// still holds the bytes the rule read, with the rule's evidence; it never
    /// overrides a reviewer and never publishes changed bytes.
    #[tokio::test]
    async fn a_download_form_is_published_only_from_an_untouched_candidate() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let actor = Uuid::new_v4();
        let form = super::super::form_check::Decision {
            publish: true,
            reason: "form",
            title: "장례용품지원신청서".into(),
            excerpt: String::new(),
            model: None,
        };
        let candidate = |url: &'static str| {
            let pool = pool.clone();
            let fp = fingerprint(&p);
            let id = p.id;
            async move {
                sqlx::query_as::<_, Resource>("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,content_hash,file_format,byte_size,checked_at,check_status)
                    VALUES($1,'template',$2,$2,'장례용품 지원 신청서.hwp',$3,'h1','hwp',10,now(),'ok') RETURNING *")
                    .bind(id).bind(url).bind(fp).fetch_one(&pool).await.unwrap()
            }
        };
        let row = candidate("https://official.example.org/forms/a.hwp").await;
        assert!(
            !publish_form(&pool, &row, "h0", actor, &form).await.unwrap(),
            "other bytes than the rule read"
        );
        assert!(publish_form(&pool, &row, "h1", actor, &form).await.unwrap());
        let now = reload(&pool, row.id).await;
        assert_eq!(
            (now.status.as_str(), now.access.as_str(), now.reviewed_by),
            ("verified", "direct", Some(actor))
        );
        assert!(now.usable(&fingerprint(&p), Utc::now()), "users see it");
        let evidence: serde_json::Value = sqlx::query_scalar(
            "SELECT evidence FROM application_resource_reviews WHERE resource_id=$1",
        )
        .bind(row.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (
                evidence["rule"].as_str(),
                evidence["validity_hours"].as_i64()
            ),
            (Some("download_form_v1"), Some(168))
        );
        // Once published (or once any reviewer decided), the rule never acts again.
        assert!(!publish_form(&pool, &now, "h1", actor, &form).await.unwrap());
        let rejected = candidate("https://official.example.org/forms/b.hwp").await;
        sqlx::query("UPDATE program_application_resources SET status='rejected' WHERE id=$1")
            .bind(rejected.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!publish_form(&pool, &rejected, "h1", actor, &form)
            .await
            .unwrap());
    }

    /// A published form whose re-checks lapsed (the site was down) renews by
    /// grace when the re-fetched file is the one the rule read: its evidence
    /// carries `content_hash`, which GRACE_WINDOW_SQL compares.
    #[tokio::test]
    async fn a_published_form_renews_by_grace_on_the_same_bytes() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let actor = Uuid::new_v4();
        let form = super::super::form_check::Decision {
            publish: true,
            reason: "form",
            title: "장례용품지원신청서".into(),
            excerpt: String::new(),
            model: None,
        };
        let row = sqlx::query_as::<_, Resource>("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,content_hash,source_hash,file_format,byte_size,checked_at,check_status)
            VALUES($1,'template','https://official.example.org/forms/grace.hwp','https://official.example.org/forms/grace.hwp','장례용품 지원 신청서.hwp',$2,'h','source','hwp',10,now(),'ok') RETURNING *")
            .bind(p.id).bind(fingerprint(&p)).fetch_one(&pool).await.unwrap();
        assert!(publish_form(&pool, &row, "h", actor, &form).await.unwrap());
        sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '190 hours', valid_until=now()-interval '30 hours' WHERE id=$1")
            .bind(row.id).execute(&pool).await.unwrap();
        let expired = reload(&pool, row.id).await;
        assert!(
            !expired.usable(&fingerprint(&p), Utc::now()),
            "hidden while expired"
        );
        let recorded = success(&pool, &expired, &p, file("h"), false)
            .await
            .unwrap();
        assert!(recorded.grace_renewed, "{recorded:?}");
        assert!(reload(&pool, row.id)
            .await
            .usable(&fingerprint(&p), Utc::now()));
        cleanup(&pool, &p).await;
    }

    /// The undo rejects only what the rule published and is still its latest
    /// review; a form a person verified by hand, or reviewed after the rule,
    /// stays as the person left it. Each undo leaves a rejected review.
    #[tokio::test]
    async fn the_undo_rejects_only_what_the_rule_published() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let rule_actor = Uuid::new_v4();
        let form = super::super::form_check::Decision {
            publish: true,
            reason: "form",
            title: "장례용품지원신청서".into(),
            excerpt: String::new(),
            model: None,
        };
        let insert = |url: &'static str| {
            let pool = pool.clone();
            let fp = fingerprint(&p);
            let id = p.id;
            async move {
                sqlx::query_as::<_, Resource>("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,content_hash,file_format,byte_size,checked_at,check_status)
                    VALUES($1,'template',$2,$2,'신청서.hwp',$3,'h1','hwp',10,now(),'ok') RETURNING *")
                    .bind(id).bind(url).bind(fp).fetch_one(&pool).await.unwrap()
            }
        };
        let by_rule = insert("https://official.example.org/forms/u1.hwp").await;
        assert!(publish_form(&pool, &by_rule, "h1", rule_actor, &form)
            .await
            .unwrap());
        // Published by the rule, then re-verified by a person.
        let reviewed = insert("https://official.example.org/forms/u2.hwp").await;
        assert!(publish_form(&pool, &reviewed, "h1", rule_actor, &form)
            .await
            .unwrap());
        let person = Uuid::new_v4();
        sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence,created_at) VALUES($1,$2,'verified',9,'{\"validity_hours\":24}',now()+interval '1 second')")
            .bind(reviewed.id).bind(person).execute(&pool).await.unwrap();
        // Verified by hand only.
        let by_hand = verified(
            &pool,
            &p,
            "template",
            "https://official.example.org/forms/u3.hwp",
        )
        .await;
        let undone = undo_download_forms(&pool, rule_actor).await.unwrap();
        assert_eq!(undone, vec![by_rule.id]);
        assert_eq!(reload(&pool, by_rule.id).await.status, "rejected");
        assert_eq!(reload(&pool, reviewed.id).await.status, "verified");
        assert_eq!(reload(&pool, by_hand.id).await.status, "verified");
        let (decision, undo): (String, Option<bool>) = sqlx::query_as(
            "SELECT decision, (evidence->>'undo')::bool FROM application_resource_reviews WHERE resource_id=$1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(by_rule.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((decision.as_str(), undo), ("rejected", Some(true)));
        cleanup(&pool, &p).await;
    }

    /// The 복지로 form listing: a programme is due once a week, its listed
    /// files become depth-0 template candidates that are their own source,
    /// and a second listing of the same files adds nothing.
    #[tokio::test]
    async fn welfare_listed_files_are_queued_once_and_the_programme_waits_a_week() {
        let Some(pool) = pool().await else { return };
        let id = Uuid::new_v4();
        let serv = format!("WLF9{}", &id.as_u128().to_string()[..7]);
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,raw_payload) VALUES($1,'welfare','local_welfare',$2,'welfare-forms-fixture','active',true,jsonb_build_object('servId',$3::text))")
            .bind(id).bind(format!("local_welfare_{serv}")).bind(&serv).execute(&pool).await.unwrap();
        let due = welfare_programs_due(&pool, "local_welfare", 5000)
            .await
            .unwrap();
        let p = due
            .into_iter()
            .find(|p| p.id == id)
            .expect("a never-asked programme is due");
        let files = vec![discovery::Candidate {
            kind: "template",
            url: format!("https://www.bokjiro.go.kr/ssis-tbu/CmmFileUtil/getDownload.do?atcflId={serv}&atcflSn=1"),
            label: "신청서.hwp".into(),
        }];
        assert_eq!(queue_files(&pool, &p, &files).await.unwrap(), 1);
        assert_eq!(queue_files(&pool, &p, &files).await.unwrap(), 0);
        let (kind, depth, same): (String, i32, bool) = sqlx::query_as(
            "SELECT kind, discovery_depth, source_url = url FROM program_application_resources WHERE program_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((kind.as_str(), depth, same), ("template", 0, true));
        welfare_forms_scanned(&pool, id).await.unwrap();
        let due = welfare_programs_due(&pool, "local_welfare", 5000)
            .await
            .unwrap();
        assert!(
            due.iter().all(|p| p.id != id),
            "asked today: not due for a week"
        );
    }

    async fn verified(pool: &PgPool, p: &Program, kind: &str, url: &str) -> Resource {
        sqlx::query_as::<_,Resource>("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,status,access,content_hash,source_hash,file_format,byte_size,checked_at,verified_at,valid_until,reviewed_by,review_note)
            VALUES($1,$2,$3,'https://official.example.org/notice','신청하기',$4,'verified','direct','old','source',
              CASE WHEN $2='template' THEN 'pdf' END,CASE WHEN $2='template' THEN 10 END,now(),now(),now()+interval '1 day',$5,'동일 사업과 현재 모집 회차의 신청 화면을 확인한 테스트 근거입니다.') RETURNING *")
            .bind(p.id).bind(kind).bind(url).bind(fingerprint(p)).bind(Uuid::new_v4()).fetch_one(pool).await.unwrap()
    }
    async fn reload(pool: &PgPool, id: Uuid) -> Resource {
        sqlx::query_as::<_, Resource>("SELECT * FROM program_application_resources WHERE id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }
    fn file(hash: &str) -> Observation {
        Observation {
            file_format: Some("hwp".into()),
            size: Some(10),
            ..observation(hash)
        }
    }
    fn observation(hash: &str) -> Observation {
        Observation {
            hash: hash.into(),
            source_hash: "source".into(),
            file_format: None,
            size: None,
            links: vec![],
            form: None,
        }
    }
    /// Registers an approved direct-collection scope; `expires` is a SQL
    /// interval from now (negative for a lapsed approval).
    async fn approve(pool: &PgPool, domain: &str, prefixes: &[&str], expires: &str) -> Uuid {
        let prefixes: Vec<String> = prefixes.iter().map(|p| p.to_string()).collect();
        sqlx::query_scalar("INSERT INTO benefit_source_domains(organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,min_request_interval_ms)
          VALUES('resource scope fixture',$1,'brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          $2,$2,'https://mazimi.example/bot','fixture',now(),now()+$3::interval,'direct_public',30000) RETURNING id")
            .bind(domain).bind(&prefixes).bind(expires).fetch_one(pool).await.unwrap()
    }
    async fn revoke(pool: &PgPool, id: Uuid) {
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
    async fn cleanup(pool: &PgPool, p: &Program) {
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(p.id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn revalidation_preserves_unchanged_evidence_and_cannot_overwrite_newer_review() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let row = verified(&pool, &p, "application", "https://form.example.org/apply").await;
        assert!(!may_renew(&row, false, Utc::now()));
        let recorded = success(&pool, &row, &p, observation("old"), false)
            .await
            .unwrap();
        assert_eq!(
            recorded,
            Recorded {
                updated: true,
                newly_changed: false,
                grace_renewed: false,
            }
        );
        let current = reload(&pool, row.id).await;
        assert_eq!(current.status, "verified");
        assert_eq!(current.check_status.as_deref(), Some("ok"));
        // Same evidence: the reviewer's optimistic lock does not move.
        assert_eq!(current.revision, row.revision);

        // A different page is flagged, not demoted, and stays flagged.
        assert!(super::super::needs_review(
            &current,
            &observation("new-intake"),
            &fingerprint(&p)
        ));
        let recorded = success(&pool, &current, &p, observation("new-intake"), true)
            .await
            .unwrap();
        assert!(recorded.newly_changed);
        let changed = reload(&pool, row.id).await;
        assert_eq!(changed.status, "verified");
        assert_eq!(changed.check_status.as_deref(), Some("changed"));
        assert!(changed.valid_until.is_some());
        assert_eq!(changed.revision, current.revision + 1);
        assert!(!may_renew(&changed, false, Utc::now()));
        let again = success(&pool, &changed, &p, observation("new-intake"), false)
            .await
            .unwrap();
        assert!(!again.newly_changed);
        assert_eq!(
            reload(&pool, row.id).await.check_status.as_deref(),
            Some("changed")
        );

        // A concurrent admin rejection cannot be undone by an in-flight probe.
        let before = reload(&pool, row.id).await;
        sqlx::query("UPDATE program_application_resources SET status='rejected',valid_until=NULL,revision=revision+1 WHERE id=$1").bind(row.id).execute(&pool).await.unwrap();
        assert!(
            !success(&pool, &before, &p, observation("old"), false)
                .await
                .unwrap()
                .updated
        );
        assert_eq!(reload(&pool, row.id).await.status, "rejected");
        cleanup(&pool, &p).await;
    }

    /// Review H1: once the programme moves to a new intake round, a probe of
    /// a verified template records the new round's fingerprint (the admin
    /// review requires a probe for this round) and flags the row `changed`,
    /// so it stays hidden until a reviewer approves it again. A page keeps
    /// its reviewed fingerprint: a changed page is still published.
    #[tokio::test]
    async fn a_new_round_probe_lets_a_verified_template_be_reapproved_but_never_publishes_it() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let template = verified(
            &pool,
            &p,
            "template",
            "https://official.example.org/form.hwp",
        )
        .await;
        let page = verified(&pool, &p, "application", "https://form.example.org/apply").await;
        sqlx::query(
            "UPDATE programs SET application_end_at = now() + interval '400 days' WHERE id=$1",
        )
        .bind(p.id)
        .execute(&pool)
        .await
        .unwrap();
        let next = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1")
            .bind(p.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let fp = fingerprint(&next);
        assert_ne!(fp, fingerprint(&p));

        let same_file = Observation {
            file_format: Some("pdf".into()),
            ..file("old")
        };
        assert!(super::super::needs_review(&template, &same_file, &fp));
        let recorded = success(&pool, &template, &next, same_file, true)
            .await
            .unwrap();
        assert!(recorded.updated && recorded.newly_changed);
        let after = reload(&pool, template.id).await;
        assert_eq!(after.program_fingerprint, fp, "probed for this round");
        assert_eq!(after.check_status.as_deref(), Some("changed"));
        assert!(after.revision > template.revision, "reviewers re-read it");
        assert!(!after.published(&fp, Utc::now()), "not shown before review");
        assert!(!after.usable(&fp, Utc::now()));
        // The next probe of the unchanged file keeps it flagged.
        success(&pool, &after, &next, file("old"), false)
            .await
            .unwrap();
        let again = reload(&pool, template.id).await;
        assert_eq!(again.check_status.as_deref(), Some("changed"));
        assert!(!again.usable(&fp, Utc::now()));

        success(&pool, &page, &next, observation("old"), true)
            .await
            .unwrap();
        let page = reload(&pool, page.id).await;
        assert_eq!(page.program_fingerprint, fingerprint(&p));
        assert!(!page.published(&fp, Utc::now()));
        cleanup(&pool, &p).await;
    }

    /// C1 regression: the bot not being allowed to look is not evidence that
    /// the destination is gone.
    #[tokio::test]
    async fn verified_row_with_current_revision_survives_every_non_gone_failure() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let row = verified(&pool, &p, "application", "https://form.example.org/apply").await;
        for error in [
            FetchError::Approval,
            FetchError::Robots,
            FetchError::Network,
            FetchError::Http(503),
            FetchError::Format,
            // Last: the off-site code it records is sticky (review M-1).
            FetchError::Redirect(None),
        ] {
            let current = reload(&pool, row.id).await;
            let f = Failure::of(&error, &current, Utc::now());
            assert!(!failure(&pool, &current, &f).await.unwrap(), "{error:?}");
            let after = reload(&pool, row.id).await;
            assert_eq!(after.status, "verified", "{error:?}");
            assert!(after.valid_until.is_some(), "{error:?}");
            assert_eq!(after.revision, row.revision, "{error:?}");
            assert_eq!(after.last_failure_code.as_deref(), Some(f.code.as_str()));
        }
        let after = reload(&pool, row.id).await;
        assert_eq!(after.consecutive_failures, 6);
        assert_eq!(after.consecutive_gone, 0);
        // Backoff grew with the failure count (6 failures: 14-day cap).
        let days: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM next_check_at-now())/86400)::float8 FROM program_application_resources WHERE id=$1")
            .bind(row.id).fetch_one(&pool).await.unwrap();
        assert!((13.9..=14.0).contains(&days), "{days}");
        cleanup(&pool, &p).await;
    }

    /// Review M7: a verified destination that now redirects off its
    /// reviewed site is hidden at once, without a demotion; the next clean
    /// probe shows it again.
    #[tokio::test]
    async fn a_verified_row_redirecting_off_site_is_hidden_until_a_clean_probe() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let fp = fingerprint(&p);
        let row = verified(&pool, &p, "notice", "https://official.example.org/notice/7").await;
        assert!(row.usable(&fp, Utc::now()));
        let offsite = FetchError::Redirect(Some("https://elsewhere.example.net/".into()));
        let f = Failure::of(&offsite, &row, Utc::now());
        assert!(!failure(&pool, &row, &f).await.unwrap(), "never demoted");
        let hidden = reload(&pool, row.id).await;
        assert_eq!(hidden.status, "verified");
        assert!(hidden.valid_until.is_some());
        assert!(
            !hidden.usable(&fp, Utc::now()),
            "hidden while it leaves the site"
        );
        // Review M-1: a later timeout does not unhide it.
        let f = Failure::of(&FetchError::Network, &hidden, Utc::now());
        failure(&pool, &hidden, &f).await.unwrap();
        let hidden = reload(&pool, row.id).await;
        assert_eq!(
            hidden.last_failure_code.as_deref(),
            Some(REDIRECTED_OFF_SITE)
        );
        assert!(
            !hidden.usable(&fp, Utc::now()),
            "still hidden after a timeout"
        );
        success(&pool, &hidden, &p, observation("old"), false)
            .await
            .unwrap();
        assert!(reload(&pool, row.id).await.usable(&fp, Utc::now()));
        // A same-site redirect is reported only; the row stays shown.
        let current = reload(&pool, row.id).await;
        let moved = FetchError::Redirect(Some("https://official.example.org/notice/7/".into()));
        let f = Failure::of(&moved, &current, Utc::now());
        failure(&pool, &current, &f).await.unwrap();
        assert!(reload(&pool, row.id).await.usable(&fp, Utc::now()));
        cleanup(&pool, &p).await;
    }

    /// Review L-4: the daily summary's `verified` counts only what users see;
    /// rows hidden by an off-site redirect (M7) or an unvouched host (M6)
    /// are counted under `hidden`.
    #[tokio::test]
    async fn the_daily_count_leaves_hidden_rows_out_of_verified() {
        let Some(pool) = pool().await else { return };
        let n = |v: &serde_json::Value, a: &str, b: &str| v[a][b].as_i64().unwrap_or(0);
        let m = |v: &serde_json::Value, r: &str, k: &str| v["hidden"][r][k].as_i64().unwrap_or(0);
        let before = daily_counts(&pool).await.unwrap();
        let p = program(&pool).await;
        verified(
            &pool,
            &p,
            "notice",
            "https://official.example.org/notice/l4",
        )
        .await;
        let moved = verified(
            &pool,
            &p,
            "notice",
            "https://official.example.org/notice/l4-moved",
        )
        .await;
        sqlx::query("UPDATE program_application_resources SET last_failure_code=$2 WHERE id=$1")
            .bind(moved.id)
            .bind(REDIRECTED_OFF_SITE)
            .execute(&pool)
            .await
            .unwrap();
        verified(
            &pool,
            &p,
            "application",
            "https://unapproved-l4.example.net/apply",
        )
        .await;
        let after = daily_counts(&pool).await.unwrap();
        cleanup(&pool, &p).await;
        assert_eq!(
            n(&after, "verified", "notice") - n(&before, "verified", "notice"),
            1
        );
        assert_eq!(
            n(&after, "verified", "application"),
            n(&before, "verified", "application")
        );
        assert_eq!(
            m(&after, "redirect_off_site", "notice") - m(&before, "redirect_off_site", "notice"),
            1
        );
        assert_eq!(
            m(&after, "unapproved_host", "application")
                - m(&before, "unapproved_host", "application"),
            1
        );
    }

    #[tokio::test]
    async fn only_three_consecutive_gone_probes_demote_and_success_resets_counters() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let row = verified(&pool, &p, "application", "https://form.example.org/apply").await;
        let gone = |r: &Resource| Failure::of(&FetchError::Http(404), r, Utc::now());
        // 404, 404, then a 503 breaks the streak.
        for _ in 0..2 {
            let current = reload(&pool, row.id).await;
            assert!(!failure(&pool, &current, &gone(&current)).await.unwrap());
        }
        let current = reload(&pool, row.id).await;
        failure(
            &pool,
            &current,
            &Failure::of(&FetchError::Http(503), &current, Utc::now()),
        )
        .await
        .unwrap();
        assert_eq!(reload(&pool, row.id).await.consecutive_gone, 0);
        // A success resets everything.
        let current = reload(&pool, row.id).await;
        success(&pool, &current, &p, observation("old"), false)
            .await
            .unwrap();
        let current = reload(&pool, row.id).await;
        assert_eq!(
            (current.consecutive_failures, current.consecutive_gone),
            (0, 0)
        );
        assert!(current.last_failure_code.is_none());
        // Three in a row demote, clear the expiry stamp and bump the revision.
        let mut demoted = vec![];
        for _ in 0..3 {
            let current = reload(&pool, row.id).await;
            demoted.push(failure(&pool, &current, &gone(&current)).await.unwrap());
        }
        assert_eq!(demoted, vec![false, false, true]);
        let after = reload(&pool, row.id).await;
        assert_eq!(after.status, "unavailable");
        assert!(after.valid_until.is_none());
        assert!(after.verified_at.is_some(), "kept as history");
        assert_eq!(after.revision, current.revision + 1);
        cleanup(&pool, &p).await;
    }

    #[tokio::test]
    async fn candidate_failures_keep_candidate_status_and_redirects_never_queue_applications() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        sqlx::query("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint) VALUES($1,'application','https://form.example.org/a','https://official.example.org/notice','신청하기',$2)")
            .bind(p.id).bind(fingerprint(&p)).execute(&pool).await.unwrap();
        let row = sqlx::query_as::<_, Resource>(
            "SELECT * FROM program_application_resources WHERE program_id=$1",
        )
        .bind(p.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        for _ in 0..4 {
            let current = reload(&pool, row.id).await;
            failure(
                &pool,
                &current,
                &Failure::of(&FetchError::Http(410), &current, Utc::now()),
            )
            .await
            .unwrap();
        }
        let after = reload(&pool, row.id).await;
        assert_eq!(after.status, "candidate");
        assert_eq!(after.check_status.as_deref(), Some("unreachable"));
        // Claim does not bump the reviewer's revision (H4).
        let scope = approve(&pool, "form.example.org", &["/"], "30 days").await;
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()-interval '1 minute' WHERE id=$1")
            .bind(row.id).execute(&pool).await.unwrap();
        let claimed = claim(&pool, 100).await.unwrap();
        let mine = claimed.iter().find(|r| r.id == row.id).expect("claimed");
        assert_eq!(mine.revision, after.revision);
        redirect_candidate(&pool, mine, &p, "https://nid.naver.com/nidlogin.login")
            .await
            .unwrap();
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM program_application_resources WHERE program_id=$1 AND url LIKE 'https://nid.%'",
        )
        .bind(p.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(kinds, vec!["notice".to_string()]);
        revoke(&pool, scope).await;
        cleanup(&pool, &p).await;
    }

    /// A3: the per-run budget must go to rows that may be fetched. Only a
    /// URL inside an approved, unexpired scope (host and path prefix) is
    /// seeded or claimed; everything else waits untouched.
    #[tokio::test]
    async fn only_urls_inside_an_approved_unexpired_scope_are_seeded_and_claimed() {
        let Some(pool) = pool().await else { return };
        let tag = Uuid::new_v4().simple().to_string();
        let (ok, lapsed, unknown) = (
            format!("ok-{tag}.example.org"),
            format!("lapsed-{tag}.example.org"),
            format!("unknown-{tag}.example.org"),
        );
        let in_scope = format!("https://{ok}/apply/1");
        let scopes = [
            approve(&pool, &ok, &["/apply"], "30 days").await,
            approve(&pool, &lapsed, &["/"], "-1 day").await,
        ];
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,raw_payload) VALUES($1,'policy','manual','resource-seed-fixture','active',true,$2,$3)")
            .bind(id)
            .bind(&in_scope)
            .bind(serde_json::json!({
                "aplyUrlAddr": format!("https://{ok}/notice/2"),
                "refUrlAddr1": format!("https://{lapsed}/apply/3"),
                "refUrlAddr2": format!("https://{unknown}/apply/4"),
            }))
            .execute(&pool)
            .await
            .unwrap();
        let p = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            discovery::seeds(&p).len(),
            4,
            "all four URLs are candidates"
        );
        seed(&pool, 100_000).await.unwrap();
        let urls = |pool: PgPool| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT url FROM program_application_resources WHERE program_id=$1 ORDER BY url",
            )
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        assert_eq!(urls(pool.clone()).await, vec![in_scope.clone()]);

        // Rows queued before this rule (or discovered as links) on other
        // scopes are never leased.
        for url in [
            format!("https://{ok}/notice/2"),
            format!("https://{lapsed}/apply/3"),
            format!("https://{unknown}/apply/4"),
        ] {
            sqlx::query("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint) VALUES($1,'notice',$2,$2,'legacy',$3)")
                .bind(id).bind(&url).bind(fingerprint(&p)).execute(&pool).await.unwrap();
        }
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()-interval '1 minute' WHERE program_id=$1")
            .bind(id).execute(&pool).await.unwrap();
        let claimed: Vec<String> = claim(&pool, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.program_id == id)
            .map(|r| r.url)
            .collect();
        assert_eq!(claimed, vec![in_scope.clone()]);
        // The lease holds: a second claim does not hand the row out again.
        assert!(!claim(&pool, 100)
            .await
            .unwrap()
            .iter()
            .any(|r| r.program_id == id));
        let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM program_application_resources WHERE program_id=$1 AND next_check_at<=now()")
            .bind(id).fetch_one(&pool).await.unwrap();
        // Unapproved hosts wait untouched; the approved host's off-prefix row
        // is moved back a day so it cannot crowd the scan window.
        assert_eq!(waiting, 2, "rows on unapproved hosts wait untouched");
        let pushed: bool = sqlx::query_scalar("SELECT next_check_at > now() + interval '23 hours' FROM program_application_resources WHERE program_id=$1 AND url=$2")
            .bind(id).bind(format!("https://{ok}/notice/2")).fetch_one(&pool).await.unwrap();
        assert!(pushed);
        for scope in scopes {
            revoke(&pool, scope).await;
        }
        cleanup(&pool, &p).await;
    }
    /// 7.1: a verified template is leased before the candidate backlog,
    /// even when its row is due later.
    #[tokio::test]
    async fn verified_templates_are_claimed_before_older_candidates() {
        let Some(pool) = pool().await else { return };
        let tag = Uuid::new_v4().simple().to_string();
        let host = format!("claim-{tag}.example.org");
        let scope = approve(&pool, &host, &["/"], "30 days").await;
        let p = program(&pool).await;
        let mut backlog = vec![];
        for n in 0..3 {
            let id: Uuid = sqlx::query_scalar("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,next_check_at)
                VALUES($1,'notice',$2,$2,'공고',$3,now()-interval '3 days') RETURNING id")
                .bind(p.id).bind(format!("https://{host}/notice/{n}")).bind(fingerprint(&p)).fetch_one(&pool).await.unwrap();
            backlog.push(id);
        }
        let template = verified(&pool, &p, "template", &format!("https://{host}/form.hwp")).await;
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()-interval '1 minute' WHERE id=$1")
            .bind(template.id).execute(&pool).await.unwrap();
        // A second verified template: claim(N) must hand both out first, in
        // order, so a run that stops early still renews them (review 1).
        let second = verified(&pool, &p, "template", &format!("https://{host}/form2.hwp")).await;
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()-interval '2 minutes' WHERE id=$1")
            .bind(second.id).execute(&pool).await.unwrap();
        let order: Vec<Uuid> = claim(&pool, 5)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(order.len(), 5, "{order:?}");
        assert_eq!(&order[..2], &[second.id, template.id], "{order:?}");
        assert!(backlog.iter().all(|id| order[2..].contains(id)));
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()-interval '1 minute' WHERE program_id=$1 AND id<>$2")
            .bind(p.id).bind(second.id).execute(&pool).await.unwrap();
        sqlx::query("UPDATE program_application_resources SET next_check_at=now()+interval '1 day' WHERE id=$1")
            .bind(second.id).execute(&pool).await.unwrap();
        // The backlog is due days earlier; claim(1) must still pick the template.
        let first = claim(&pool, 1).await.unwrap();
        assert_eq!(
            first.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![template.id]
        );
        revoke(&pool, scope).await;
        cleanup(&pool, &p).await;
    }

    /// 7.1: a successful re-check keeps the reviewer's validity window
    /// instead of shrinking a 168-hour approval to one day.
    #[tokio::test]
    async fn renewal_keeps_the_approved_validity_window() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let hours = |pool: PgPool, id: Uuid| async move {
            sqlx::query_scalar::<_, f64>("SELECT (extract(epoch FROM valid_until-now())/3600)::float8 FROM program_application_resources WHERE id=$1")
                .bind(id).fetch_one(&pool).await.unwrap()
        };
        let row = verified(&pool, &p, "template", "https://form.example.org/a.hwp").await;
        sqlx::query("UPDATE program_application_resources SET content_hash='h' WHERE id=$1")
            .bind(row.id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,'verified',1,$3)")
            .bind(row.id).bind(Uuid::new_v4()).bind(serde_json::json!({"validity_hours":168})).execute(&pool).await.unwrap();
        let row = reload(&pool, row.id).await;
        assert!(may_renew(&row, false, Utc::now()));
        success(&pool, &row, &p, file("h"), false).await.unwrap();
        let after = hours(pool.clone(), row.id).await;
        assert!((167.9..=168.0).contains(&after), "{after}");
        // No review record: the row's own window (here 1 day) is kept.
        let other = verified(&pool, &p, "template", "https://form.example.org/b.hwp").await;
        sqlx::query("UPDATE program_application_resources SET content_hash='h' WHERE id=$1")
            .bind(other.id)
            .execute(&pool)
            .await
            .unwrap();
        let other = reload(&pool, other.id).await;
        success(&pool, &other, &p, file("h"), false).await.unwrap();
        let after = hours(pool.clone(), other.id).await;
        assert!((23.9..=24.0).contains(&after), "{after}");
        // A forged evidence value cannot exceed the 7-day ceiling.
        sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,'verified',1,$3)")
            .bind(other.id).bind(Uuid::new_v4()).bind(serde_json::json!({"validity_hours":9999})).execute(&pool).await.unwrap();
        let other = reload(&pool, other.id).await;
        success(&pool, &other, &p, file("h"), false).await.unwrap();
        let after = hours(pool.clone(), other.id).await;
        assert!((167.9..=168.0).contains(&after), "{after}");
        cleanup(&pool, &p).await;
    }

    /// 7.2: recruitment documents, board-wide attachments, earlier-round
    /// files and files from unrelated linked pages are marked rejected with
    /// a reason; related, reviewed and unknown-caption rows are kept; the
    /// dry run writes nothing.
    #[tokio::test]
    async fn noise_templates_are_marked_rejected_with_a_reason_never_deleted() {
        let Some(pool) = pool().await else { return };
        let tag = Uuid::new_v4().simple().to_string();
        let mk = |title: &'static str, active: bool| {
            let pool = pool.clone();
            async move {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url) VALUES($1,'policy','manual',$2,'active',$3,'https://official.example.org/notice')")
                    .bind(id).bind(title).bind(active).execute(&pool).await.unwrap();
                sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id=$1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let add = |p: &Program,
                   kind: &'static str,
                   url: String,
                   source: String,
                   label: &'static str,
                   depth: i32| {
            let pool = pool.clone();
            let (id, fp) = (p.id, fingerprint(p));
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint,discovery_depth)
                    VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id")
                    .bind(id).bind(kind).bind(url).bind(source).bind(label).bind(fp).bind(depth).fetch_one(&pool).await.unwrap()
            }
        };
        let base = format!("https://noise-{tag}.example.org");
        let marriage = mk("결혼축하금 지원사업", true).await;
        let birth = mk("출산장려금 지원사업", true).await;
        let farm = mk("청년희망 스마트팜 확산지원", true).await;
        let closed = mk("청년 우울증 치료비 지원 1차", false).await;
        // Round 1 ended (window over); merely inactive is not "earlier".
        sqlx::query("UPDATE programs SET application_end_at=now()-interval '30 days' WHERE id=$1")
            .bind(closed.id)
            .execute(&pool)
            .await
            .unwrap();
        let depression = mk("청년 우울증 치료비 지원 추가모집", true).await;
        let suit = mk("청년 면접 정장 지원", true).await;
        // A board post about a recruitment exam, linked from each programme page.
        let board = format!("{base}/board/exam");
        for p in [&marriage, &birth, &farm] {
            add(
                p,
                "notice",
                board.clone(),
                format!("{base}/p/{}", p.id),
                "new 행정지원과 2026년 제5회 임기제공무원 채용시험 시행계획 공고",
                1,
            )
            .await;
        }
        let mut shared = vec![];
        for p in [&marriage, &birth, &farm] {
            shared.push(
                add(
                    p,
                    "template",
                    format!("{base}/f/exam-forms.hwp"),
                    board.clone(),
                    "제출서류 일체.hwp",
                    2,
                )
                .await,
            );
        }
        let recruit = add(
            &suit,
            "template",
            format!("{base}/f/id.hwp"),
            format!("{base}/p/suit"),
            "(서식1) 신원진술서 제20호서식.hwp",
            1,
        )
        .await;
        // Round 1's notice, reached through a board link (depth 2), as in
        // production; a depth-1 copy of a stable yearly form would be kept.
        let round = add(
            &depression,
            "template",
            format!("{base}/f/round1.hwp"),
            format!("{base}/p/dep"),
            "신청서.hwp",
            2,
        )
        .await;
        add(
            &closed,
            "template",
            format!("{base}/f/round1.hwp"),
            format!("{base}/p/dep1"),
            "신청서.hwp",
            1,
        )
        .await;
        let unrelated_page = format!("{base}/board/survey");
        add(
            &suit,
            "notice",
            unrelated_page.clone(),
            format!("{base}/p/suit"),
            "2027년 대중소기업 지역대기질 개선 상생협력 시범사업 공고",
            1,
        )
        .await;
        let unrelated = add(
            &suit,
            "template",
            format!("{base}/f/air.hwp"),
            unrelated_page,
            "신청서.hwp",
            2,
        )
        .await;
        let own_page = format!("{base}/board/suit");
        add(
            &suit,
            "notice",
            own_page.clone(),
            format!("{base}/p/suit"),
            "2026년 청년 면접정장 무료대여 모집공고",
            1,
        )
        .await;
        let kept_related = add(
            &suit,
            "template",
            format!("{base}/f/suit.hwp"),
            own_page,
            "신청서.hwp",
            2,
        )
        .await;
        let generic_page = format!("{base}/board/generic");
        add(
            &suit,
            "notice",
            generic_page.clone(),
            format!("{base}/p/suit"),
            "모집공고",
            1,
        )
        .await;
        let kept_generic = add(
            &suit,
            "template",
            format!("{base}/f/generic.hwp"),
            generic_page,
            "신청서.hwp",
            2,
        )
        .await;
        let kept_own = add(
            &suit,
            "template",
            format!("{base}/f/own.hwp"),
            format!("{base}/p/suit"),
            "신청서.hwp",
            1,
        )
        .await;

        let mine = |sweep: NoiseSweep| {
            let mut out: Vec<(Uuid, &'static str)> = sweep
                .rejected
                .into_iter()
                .map(|(r, why)| (r.id, why))
                .filter(|(id, _)| {
                    shared.contains(id)
                        || [
                            recruit,
                            round,
                            unrelated,
                            kept_related,
                            kept_generic,
                            kept_own,
                        ]
                        .contains(id)
                })
                .collect();
            out.sort();
            out
        };
        let mut expected: Vec<(Uuid, &'static str)> =
            shared.iter().map(|id| (*id, "shared_attachment")).collect();
        expected.extend([
            (recruit, "recruitment_document"),
            (round, "earlier_round"),
            (unrelated, "unrelated_source_page"),
        ]);
        expected.sort();
        let dry = mine(reject_noise(&pool, false).await.unwrap());
        assert_eq!(dry, expected);
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM program_application_resources WHERE status='rejected' AND url LIKE $1")
            .bind(format!("{base}%")).fetch_one(&pool).await.unwrap();
        assert_eq!(before, 0, "dry run writes nothing");
        // A reviewed row is never auto-rejected, whatever it looks like.
        sqlx::query("UPDATE program_application_resources SET status='verified',access='direct',verified_at=now(),valid_until=now()+interval '1 day',reviewed_by=$2,
            review_note='동일 사업과 현재 모집 회차의 신청 양식을 확인한 테스트 근거입니다.',content_hash='h',file_format='hwp',byte_size=10,checked_at=now() WHERE id=$1")
            .bind(recruit).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        expected.retain(|(id, _)| *id != recruit);
        let applied = mine(reject_noise(&pool, true).await.unwrap());
        assert_eq!(applied, expected);
        for (id, why) in &expected {
            let row = reload(&pool, *id).await;
            assert_eq!(row.status, "rejected");
            assert_eq!(
                row.check_error.as_deref(),
                Some(format!("auto_rejected:{why}").as_str())
            );
            assert!(row.review_note.is_some() && row.reviewed_by.is_none());
        }
        assert_eq!(reload(&pool, recruit).await.status, "verified");
        for id in [kept_related, kept_generic, kept_own] {
            assert_eq!(reload(&pool, id).await.status, "candidate");
        }
        // Idempotent; rows are kept as history (nothing deleted).
        assert!(mine(reject_noise(&pool, true).await.unwrap()).is_empty());
        // Reversible (review H1): when the evidence no longer supports a rule
        // (round 1 turns out to be still open), the automatic rejection is
        // undone on the next sweep and the row is due for a probe.
        sqlx::query("UPDATE programs SET application_end_at=now()+interval '30 days' WHERE id=$1")
            .bind(closed.id)
            .execute(&pool)
            .await
            .unwrap();
        let sweep = reject_noise(&pool, true).await.unwrap();
        assert!(sweep.restored.iter().any(|r| r.id == round), "{sweep:?}");
        let back = reload(&pool, round).await;
        assert_eq!(back.status, "candidate");
        assert!(back.check_error.is_none() && back.review_note.is_none());
        // A merely inactive programme (not ended) never makes a round "earlier".
        sqlx::query("UPDATE programs SET application_end_at=NULL WHERE id=$1")
            .bind(closed.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!mine(reject_noise(&pool, false).await.unwrap())
            .iter()
            .any(|(id, _)| *id == round));
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM program_application_resources WHERE url LIKE $1",
        )
        .bind(format!("{base}%"))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(total, 16);
        for p in [&marriage, &birth, &farm, &closed, &depression, &suit] {
            cleanup(&pool, p).await;
        }
    }

    /// Self-reporting: a verified template without a successful re-check
    /// for 48 hours opens one warning that resolves after a renewal, and
    /// one daily summary exists per UTC day.
    #[tokio::test]
    async fn at_risk_templates_open_and_resolve_an_event_and_a_daily_summary_is_kept() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let row = verified(&pool, &p, "template", "https://form.example.org/risk.hwp").await;
        sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '3 days',valid_until=now()+interval '1 day',check_status='unreachable',last_failure_code='network_timeout_or_failure' WHERE id=$1")
            .bind(row.id).execute(&pool).await.unwrap();
        let fresh = verified(&pool, &p, "template", "https://form.example.org/fresh.hwp").await;
        super::super::report_health(&pool).await.unwrap();
        let state = |pool: PgPool, id: Uuid| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM operational_events WHERE fingerprint=$1",
            )
            .bind(format!("application_resource:at_risk:{id}"))
            .fetch_optional(&pool)
            .await
            .unwrap()
        };
        assert_eq!(state(pool.clone(), row.id).await.as_deref(), Some("open"));
        assert_eq!(state(pool.clone(), fresh.id).await, None);
        let details: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM operational_events WHERE fingerprint=$1")
                .bind(format!("application_resource:at_risk:{}", row.id))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(details["expired"], false);
        assert_eq!(details["last_failure_code"], "network_timeout_or_failure");
        let day = format!(
            "application_resources:daily:{}",
            Utc::now().format("%Y-%m-%d")
        );
        let summary: serde_json::Value =
            sqlx::query_scalar("SELECT details FROM operational_events WHERE fingerprint=$1")
                .bind(&day)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            summary["verified"]["template"].as_i64().unwrap() >= 2,
            "{summary}"
        );
        assert!(
            summary["found_24h"]["template"].as_i64().unwrap() >= 2,
            "{summary}"
        );
        // The same day does not record a second summary.
        super::super::report_health(&pool).await.unwrap();
        let count: i32 = sqlx::query_scalar(
            "SELECT occurrence_count FROM operational_events WHERE fingerprint=$1",
        )
        .bind(&day)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        // A successful renewal resolves the warning.
        sqlx::query("UPDATE program_application_resources SET verified_at=now(),check_status='ok' WHERE id=$1").bind(row.id).execute(&pool).await.unwrap();
        super::super::report_health(&pool).await.unwrap();
        assert_eq!(
            state(pool.clone(), row.id).await.as_deref(),
            Some("resolved")
        );
        cleanup(&pool, &p).await;
    }

    /// 10-18 regression: a verified 24-hour template whose first probe hits
    /// a network failure is checked again well before it expires, not a day
    /// later.
    #[tokio::test]
    async fn a_transient_failure_on_a_24_hour_window_rechecks_before_expiry() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let row = verified(&pool, &p, "template", "https://form.example.org/w24.hwp").await;
        sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '13 hours',valid_until=now()+interval '11 hours' WHERE id=$1")
            .bind(row.id).execute(&pool).await.unwrap();
        for error in [
            FetchError::Network,
            FetchError::Http(503),
            FetchError::Network,
        ] {
            let current = reload(&pool, row.id).await;
            let f = Failure::of(&error, &current, Utc::now());
            failure(&pool, &current, &f).await.unwrap();
            let after = reload(&pool, row.id).await;
            let (next, until): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
                "SELECT next_check_at, valid_until FROM program_application_resources WHERE id=$1",
            )
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(next < until, "{error:?}: next {next} after expiry {until}");
            assert_eq!(after.status, "verified");
        }
        // Steps 1h, 3h, then 6h: the third retry still lands before expiry.
        let hours: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM next_check_at-now())/3600)::float8 FROM program_application_resources WHERE id=$1")
            .bind(row.id).fetch_one(&pool).await.unwrap();
        assert!((5.9..=6.0).contains(&hours), "{hours}");
        // Negative evidence keeps the plain backoff (fail-closed).
        let current = reload(&pool, row.id).await;
        let f = Failure::of(&FetchError::Http(404), &current, Utc::now());
        failure(&pool, &current, &f).await.unwrap();
        let days: f64 = sqlx::query_scalar("SELECT (extract(epoch FROM next_check_at-now())/86400)::float8 FROM program_application_resources WHERE id=$1")
            .bind(row.id).fetch_one(&pool).await.unwrap();
        assert!((7.9..=8.0).contains(&days), "{days}");
        cleanup(&pool, &p).await;
    }

    /// Grace renewal: an expired template whose re-fetched file and source
    /// page match the review exactly renews to the approved window within
    /// 72 hours, and not after; a hash mismatch never renews.
    #[tokio::test]
    async fn expired_template_with_identical_hashes_renews_within_72_hours_only() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let expired = |pool: PgPool, url: &'static str, ago: &'static str| {
            let p = p.clone();
            async move {
                let row = verified(&pool, &p, "template", url).await;
                sqlx::query(&format!(
                    "UPDATE program_application_resources SET content_hash='h', file_format='hwp', check_status='ok',
                       verified_at=now()-interval '{ago}'-interval '24 hours', valid_until=now()-interval '{ago}' WHERE id=$1"
                ))
                .bind(row.id)
                .execute(&pool)
                .await
                .unwrap();
                sqlx::query("INSERT INTO application_resource_reviews(resource_id,reviewer_id,decision,revision,evidence) VALUES($1,$2,'verified',1,$3)")
                    .bind(row.id).bind(Uuid::new_v4()).bind(serde_json::json!({"validity_hours":168,"content_hash":"h"}))
                    .execute(&pool).await.unwrap();
                reload(&pool, row.id).await
            }
        };
        let hours = |pool: PgPool, id: Uuid| async move {
            sqlx::query_scalar::<_, f64>("SELECT (extract(epoch FROM valid_until-now())/3600)::float8 FROM program_application_resources WHERE id=$1")
                .bind(id).fetch_one(&pool).await.unwrap()
        };
        let now = Utc::now();

        // Expired 30 hours ago, same bytes: renewed to the approved 168 hours.
        let row = expired(pool.clone(), "https://form.example.org/g1.hwp", "30 hours").await;
        assert!(!row.usable(&fingerprint(&p), now), "hidden while expired");
        let recorded = success(&pool, &row, &p, file("h"), false).await.unwrap();
        assert!(recorded.grace_renewed, "{recorded:?}");
        let after = reload(&pool, row.id).await;
        assert!((167.9..=168.0).contains(&hours(pool.clone(), row.id).await));
        assert!(after.usable(&fingerprint(&p), Utc::now()));
        assert_eq!(after.revision, row.revision, "same evidence, same lock");

        // Expired 73 hours ago: a reviewer must look again.
        let old = expired(pool.clone(), "https://form.example.org/g2.hwp", "73 hours").await;
        let recorded = success(&pool, &old, &p, file("h"), false).await.unwrap();
        assert!(!recorded.grace_renewed);
        assert!(hours(pool.clone(), old.id).await < -72.0);

        // A different file is flagged changed and never renews, not even later.
        let other = expired(pool.clone(), "https://form.example.org/g3.hwp", "2 hours").await;
        let observed = file("different");
        assert!(super::super::needs_review(
            &other,
            &observed,
            &fingerprint(&p)
        ));
        let recorded = success(&pool, &other, &p, observed, true).await.unwrap();
        assert!(recorded.newly_changed && !recorded.grace_renewed);
        assert!(hours(pool.clone(), other.id).await < 0.0);
        let flagged = reload(&pool, other.id).await;
        assert_eq!(flagged.check_status.as_deref(), Some("changed"));
        let recorded = success(&pool, &flagged, &p, file("different"), false)
            .await
            .unwrap();
        assert!(!recorded.grace_renewed);
        assert!(hours(pool.clone(), other.id).await < 0.0);

        // A different source page never renews either.
        let source = expired(pool.clone(), "https://form.example.org/g4.hwp", "2 hours").await;
        let moved = Observation {
            source_hash: "moved".into(),
            ..file("h")
        };
        let recorded = success(&pool, &source, &p, moved, true).await.unwrap();
        assert!(!recorded.grace_renewed);
        assert!(hours(pool.clone(), source.id).await < 0.0);

        // The review saw other bytes (or recorded none): no renewal.
        let unreviewed = expired(pool.clone(), "https://form.example.org/g5.hwp", "2 hours").await;
        sqlx::query("UPDATE application_resource_reviews SET evidence=evidence-'content_hash' WHERE resource_id=$1")
            .bind(unreviewed.id).execute(&pool).await.unwrap();
        let recorded = success(&pool, &unreviewed, &p, file("h"), false)
            .await
            .unwrap();
        assert!(!recorded.grace_renewed);
        assert!(hours(pool.clone(), unreviewed.id).await < 0.0);
        cleanup(&pool, &p).await;
    }

    /// The at-risk warning comes before expiry: a fresh success with a
    /// window that ends within 12 hours is already at risk.
    #[tokio::test]
    async fn at_risk_fires_before_the_window_ends() {
        let Some(pool) = pool().await else { return };
        let p = program(&pool).await;
        let soon = verified(&pool, &p, "template", "https://form.example.org/soon.hwp").await;
        sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '13 hours',valid_until=now()+interval '11 hours' WHERE id=$1")
            .bind(soon.id).execute(&pool).await.unwrap();
        let healthy = verified(&pool, &p, "template", "https://form.example.org/ok.hwp").await;
        let risks: Vec<Uuid> = at_risk_templates(&pool, 48, 12)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert!(risks.contains(&soon.id));
        assert!(!risks.contains(&healthy.id));
        super::super::report_health(&pool).await.unwrap();
        let details: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM operational_events WHERE fingerprint=$1 AND status='open'",
        )
        .bind(format!("application_resource:at_risk:{}", soon.id))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(details["expired"], false);
        assert!(details["hours_until_expiry"].as_f64().unwrap() > 10.0);
        cleanup(&pool, &p).await;
    }
}
