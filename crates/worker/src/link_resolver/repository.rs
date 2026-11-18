//! Storage of candidates, their checks and each programme's selection.
use super::candidates::{self, Extracted, Origin, ONE_HOP_FIELD, ONE_HOP_RANK};
use super::classify::{self, Context, Observation, PageFacts};
use super::resolve::{self, CandidateView, CheckState, Choice, Liveness, Status};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{types::Json, FromRow, PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Bumped when extraction or origin rules change, so every programme's
/// candidates are re-derived at its next sync.
pub const RESOLVER_VERSION: &str = "link_resolver_v2";
/// One-hop links taken from one notice page.
const MAX_ONE_HOP: usize = 3;

#[derive(Debug, Clone, FromRow)]
pub struct Row {
    pub id: Uuid,
    pub program_id: Uuid,
    pub field: String,
    pub field_rank: i16,
    pub url: String,
    pub host: String,
    pub origin: String,
    pub free_text: bool,
    pub source_url: Option<String>,
    pub kind: Option<String>,
    pub sub: Option<String>,
    pub access: String,
    pub check_status: String,
    pub final_url: Option<String>,
    pub http_status: Option<i16>,
    pub page: Option<Json<PageFacts>>,
    pub last_failure_code: Option<String>,
    pub last_pass_at: Option<DateTime<Utc>>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub fail_count: i32,
    pub first_fail_at: Option<DateTime<Utc>>,
    pub soft_fail_count: i32,
    pub first_soft_fail_at: Option<DateTime<Utc>>,
    pub restore_passes: i32,
    pub demoted: bool,
    /// The programme's title (`programs.title`).
    pub program_title: String,
    /// For a one-hop row: the title of the notice page it was found on.
    pub notice_title: Option<String>,
}

const ROW_COLUMNS: &str = "c.id,c.program_id,c.field,c.field_rank,c.url,c.host,c.origin,c.free_text,c.source_url,
    c.kind,c.sub,c.access,c.check_status,c.final_url,c.http_status,c.page,c.last_failure_code,c.last_pass_at,
    c.next_check_at,c.fail_count,c.first_fail_at,c.soft_fail_count,c.first_soft_fail_at,c.restore_passes,c.demoted,
    p.title AS program_title,
    (SELECT n.page->>'title' FROM program_link_candidates n WHERE n.program_id=c.program_id
       AND n.url=c.source_url AND n.field<>'notice_apply_link' LIMIT 1) AS notice_title";

impl Row {
    pub fn state(&self) -> CheckState {
        CheckState {
            status: Status::parse(&self.check_status),
            hard_failures: self.fail_count,
            first_hard_failure_at: self.first_fail_at,
            soft_failures: self.soft_fail_count,
            first_soft_failure_at: self.first_soft_fail_at,
            restore_passes: self.restore_passes,
            demoted: self.demoted,
        }
    }
    pub fn view(&self) -> CandidateView<'_> {
        CandidateView {
            url: &self.url,
            field: &self.field,
            origin: Origin::parse(&self.origin),
            free_text: self.free_text,
            programme: &self.program_title,
            notice_title: self.notice_title.as_deref(),
        }
    }
    /// The observation the shown kind rests on: robots block, else the last
    /// check that passed, else nothing (URL pattern only).
    pub fn observation(&self) -> Option<Observation> {
        if Status::parse(&self.check_status) == Status::Blocked {
            return Some(Observation::Blocked);
        }
        match (self.last_pass_at, self.http_status, &self.final_url) {
            (Some(_), Some(status), Some(final_url)) => Some(Observation::Fetched {
                status: status as u16,
                final_url: final_url.clone(),
                page: self.page.as_ref().map(|p| p.0.clone()),
            }),
            _ => None,
        }
    }
}

/// Never fetched: portal pages and links whose origin is rejected.
fn skipped(url: &str, origin: Origin) -> bool {
    origin == Origin::Rejected || classify::portal_kind(url).is_some()
}

/// Approved source domains (the allowlist half of the origin rule).
pub async fn approved_domains(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT domain FROM benefit_source_domains WHERE is_active AND verification_status='verified'
           AND compliance_status='approved' AND approval_expires_at>now()",
    )
    .fetch_all(pool)
    .await
}

#[derive(FromRow)]
struct ProgramInput {
    id: Uuid,
    source_type: String,
    official_url: Option<String>,
    raw_payload: Option<Value>,
    detail: Option<Value>,
    input_hash: Option<String>,
}

pub fn input_hash(extracted: &Extracted) -> String {
    let items: Vec<Value> = extracted
        .candidates
        .iter()
        .map(|c| serde_json::json!([c.field, c.field_rank, c.url, c.origin.as_str(), c.free_text]))
        .collect();
    mazimi_core::application_resources::digest(
        serde_json::json!({"v": RESOLVER_VERSION, "c": items})
            .to_string()
            .as_bytes(),
    )
}

#[derive(Debug, Default, serde::Serialize)]
pub struct SyncSummary {
    pub examined: usize,
    pub changed: usize,
    /// Programmes whose store failed (logged; retried at their next sync).
    pub failed: usize,
}

/// Re-derives candidates of up to `limit` programmes: never-synced first,
/// then those updated since their last sync, then the oldest sync. A
/// programme whose candidate inputs changed has its links re-checked now.
pub async fn sync(pool: &PgPool, limit: i64, approved: &[String]) -> anyhow::Result<SyncSummary> {
    sync_only(pool, None, limit, approved).await
}

/// [`sync`] restricted to `only` when given.
pub async fn sync_only(
    pool: &PgPool,
    only: Option<&[Uuid]>,
    limit: i64,
    approved: &[String],
) -> anyhow::Result<SyncSummary> {
    let programs: Vec<ProgramInput> = sqlx::query_as(
        "SELECT p.id, p.source_type, p.official_url, p.raw_payload,
                CASE WHEN p.source_type='gov_benefits' THEN s.raw_payload->'detail' END AS detail,
                ls.input_hash
         FROM current_programs p
         LEFT JOIN program_link_selections ls ON ls.program_id=p.id
         LEFT JOIN source_snapshots s ON s.source_name=p.source_type AND s.source_id=p.source_id
         WHERE ($2::uuid[] IS NULL OR p.id=ANY($2))
         ORDER BY (ls.program_id IS NULL) DESC, (p.updated_at > ls.synced_at) DESC, ls.synced_at ASC NULLS FIRST, p.id
         LIMIT $1",
    )
    .bind(limit)
    .bind(only)
    .fetch_all(pool)
    .await?;
    let mut summary = SyncSummary::default();
    let mut unchanged = Vec::new();
    for p in programs {
        summary.examined += 1;
        let empty = Value::Null;
        let raw = p.raw_payload.as_ref().unwrap_or(&empty);
        // The snapshot always carries the latest read; `detail` is absent
        // when the gov24 detail call failed (kept, not removed).
        let detail = p.detail.as_ref().filter(|d| d.is_object());
        let mut extracted = candidates::extract(
            &p.source_type,
            p.official_url.as_deref(),
            raw,
            detail,
            approved,
        );
        // A field missing from this read counts as its stored candidates, so
        // a failed detail read changes no hash and re-checks nothing.
        if !extracted.unknown_fields.is_empty() {
            keep_unknown(pool, p.id, &mut extracted).await?;
        }
        let hash = input_hash(&extracted);
        if p.input_hash.as_deref() == Some(hash.as_str()) {
            unchanged.push(p.id);
            continue;
        }
        summary.changed += 1;
        let stored = async {
            store_candidates(pool, p.id, &extracted, &hash).await?;
            reselect(pool, p.id).await
        };
        if let Err(error) = stored.await {
            // One bad programme must not stop the run (code review H2).
            summary.failed += 1;
            tracing::warn!(program_id = %p.id, error = %crate::error_safety::persisted_error(&error), "link candidates not stored");
        }
    }
    if !unchanged.is_empty() {
        sqlx::query("UPDATE program_link_selections SET synced_at=now() WHERE program_id=ANY($1)")
            .bind(&unchanged)
            .execute(pool)
            .await?;
    }
    Ok(summary)
}

/// Adds the stored candidates of `extracted.unknown_fields` to `extracted`.
async fn keep_unknown(
    pool: &PgPool,
    program: Uuid,
    extracted: &mut Extracted,
) -> Result<(), sqlx::Error> {
    let rows: Vec<(String, i16, String, String, bool)> = sqlx::query_as(
        "SELECT field, field_rank, url, origin, free_text FROM program_link_candidates
         WHERE program_id=$1 AND field=ANY($2) ORDER BY field_rank",
    )
    .bind(program)
    .bind(&extracted.unknown_fields)
    .fetch_all(pool)
    .await?;
    for (field, rank, url, origin, free_text) in rows {
        let Some(field) = extracted
            .unknown_fields
            .iter()
            .find(|f| **f == field)
            .copied()
        else {
            continue;
        };
        if extracted.candidates.iter().any(|c| c.url == url) {
            continue;
        }
        extracted.candidates.push(candidates::Candidate {
            field,
            field_rank: rank,
            url,
            origin: Origin::parse(&origin),
            free_text,
        });
    }
    extracted.candidates.sort_by_key(|c| c.field_rank);
    Ok(())
}

/// Serialises every change to one programme's candidates and selection
/// (code review H1): lanes on different hosts touch the same programme.
async fn lock_programme(tx: &mut PgConnection, program: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('program_link:' || $1::text, 0))")
        .bind(program)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

async fn store_candidates(
    pool: &PgPool,
    program: Uuid,
    extracted: &Extracted,
    hash: &str,
) -> Result<(), sqlx::Error> {
    let c = &extracted.candidates;
    let fields: Vec<&str> = c.iter().map(|c| c.field).collect();
    let ranks: Vec<i16> = c.iter().map(|c| c.field_rank).collect();
    let urls: Vec<&str> = c.iter().map(|c| c.url.as_str()).collect();
    let hosts: Vec<String> = c.iter().map(|c| classify::host_of(&c.url)).collect();
    let origins: Vec<&str> = c.iter().map(|c| c.origin.as_str()).collect();
    let free: Vec<bool> = c.iter().map(|c| c.free_text).collect();
    let skip: Vec<bool> = c.iter().map(|c| skipped(&c.url, c.origin)).collect();
    let mut tx = pool.begin().await?;
    lock_programme(&mut tx, program).await?;
    // Content changed: every kept link is checked again now.
    sqlx::query(
        "INSERT INTO program_link_candidates(program_id,field,field_rank,url,host,origin,free_text,check_status,next_check_at)
         SELECT $1,f,r,u,h,o,t,CASE WHEN s THEN 'skipped' ELSE 'unchecked' END,CASE WHEN s THEN NULL ELSE now() END
         FROM UNNEST($2::text[],$3::smallint[],$4::text[],$5::text[],$6::text[],$7::bool[],$8::bool[]) AS x(f,r,u,h,o,t,s)
         ON CONFLICT(program_id,field,url) DO UPDATE SET field_rank=excluded.field_rank,host=excluded.host,
           origin=excluded.origin,free_text=excluded.free_text,updated_at=now(),
           check_status=CASE WHEN excluded.check_status='skipped' THEN 'skipped'
                             WHEN program_link_candidates.check_status='skipped' THEN 'unchecked'
                             ELSE program_link_candidates.check_status END,
           next_check_at=excluded.next_check_at",
    )
    .bind(program)
    .bind(&fields)
    .bind(&ranks)
    .bind(&urls)
    .bind(&hosts)
    .bind(&origins)
    .bind(&free)
    .bind(&skip)
    .execute(&mut *tx)
    .await?;
    let unknown: Vec<&str> = extracted.unknown_fields.to_vec();
    sqlx::query(
        "DELETE FROM program_link_candidates c WHERE c.program_id=$1 AND c.field<>$5
           AND NOT (c.field=ANY($4))
           AND NOT EXISTS (SELECT 1 FROM UNNEST($2::text[],$3::text[]) AS x(f,u) WHERE x.f=c.field AND x.u=c.url)",
    )
    .bind(program)
    .bind(&fields)
    .bind(&urls)
    .bind(&unknown)
    .bind(ONE_HOP_FIELD)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO program_link_selections(program_id,input_hash,synced_at,state,reselect_due) VALUES($1,$2,now(),'pending',true)
         ON CONFLICT(program_id) DO UPDATE SET input_hash=excluded.input_hash,synced_at=now(),reselect_due=true",
    )
    .bind(program)
    .bind(hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

async fn rows_of(pool: &mut PgConnection, program: Uuid) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ROW_COLUMNS} FROM program_link_candidates c JOIN programs p ON p.id=c.program_id
         WHERE c.program_id=$1 ORDER BY c.field_rank, c.id"
    ))
    .bind(program)
    .fetch_all(pool)
    .await
}

/// Distinct programme titles (whitespace removed, first 30 characters, as
/// the audit counted) that list each URL.
async fn shared_counts(pool: &PgPool, urls: &[&str]) -> Result<HashMap<String, i64>, sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT c.url, count(DISTINCT left(regexp_replace(p.title,'\\s','','g'),30))
         FROM program_link_candidates c JOIN current_programs p ON p.id=c.program_id
         WHERE c.url=ANY($1) GROUP BY c.url",
    )
    .bind(urls)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Checked pages per host that carry a link with each href (distinct
/// page and link text pairs, as the audit counted): a link on three or more
/// pages of a host is site navigation.
async fn nav_counts(
    pool: &PgPool,
    hosts: &[&str],
    hrefs: &[&str],
) -> Result<HashMap<(String, String), i64>, sqlx::Error> {
    if hrefs.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT c.host, e->>1, count(DISTINCT (c.url, e->>0))
         FROM program_link_candidates c, jsonb_array_elements(c.page->'apply_links') e
         WHERE c.host=ANY($1) AND jsonb_typeof(c.page->'apply_links')='array' AND e->>1=ANY($2)
         GROUP BY c.host, e->>1",
    )
    .bind(hosts)
    .bind(hrefs)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(h, href, n)| ((h, href), n))
        .collect())
}

/// Recomputes how each candidate is shown and which one the programme
/// shows, then keeps the one-hop candidates of its notice page current.
pub async fn reselect(pool: &PgPool, program: Uuid) -> anyhow::Result<()> {
    // A one-hop link the notice page dropped may have been the selected
    // one: select once more after the candidates changed.
    for _ in 0..2 {
        if !select_once(pool, program).await? {
            break;
        }
    }
    Ok(())
}

/// One selection pass; `true` when the one-hop candidates changed.
async fn select_once(pool: &PgPool, program: Uuid) -> anyhow::Result<bool> {
    // Reads and writes of one pass share a transaction under the
    // programme's lock, so a later pass always sees this one's result.
    let mut tx = pool.begin().await?;
    lock_programme(&mut tx, program).await?;
    let rows = rows_of(&mut tx, program).await?;
    let current: Option<(Option<Uuid>, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT selected_id, notice_id, state FROM program_link_selections WHERE program_id=$1",
    )
    .bind(program)
    .fetch_optional(&mut *tx)
    .await?;
    let (current_selected, current_notice) = current
        .as_ref()
        .map(|(s, n, _)| (*s, *n))
        .unwrap_or((None, None));
    let urls: Vec<&str> = rows.iter().map(|r| r.url.as_str()).collect();
    let shared = shared_counts(pool, &urls).await?;
    let hosts: Vec<&str> = rows.iter().map(|r| r.host.as_str()).collect();
    let hrefs: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.page.as_ref())
        .flat_map(|p| p.0.apply_links.iter().map(|(_, h)| h.as_str()))
        .collect();
    let nav = nav_counts(pool, &hosts, &hrefs).await?;
    let mut choices = Vec::with_capacity(rows.len());
    for row in &rows {
        let host = row.host.clone();
        let count = |href: &str| -> usize {
            nav.get(&(host.clone(), href.to_string()))
                .copied()
                .unwrap_or(0) as usize
        };
        let ctx = Context {
            shared: shared.get(&row.url).copied().unwrap_or(0) >= 2,
            nav_count: &count,
        };
        let state = row.state();
        let shown = match state.status {
            Status::Unchecked | Status::Broken => None,
            _ => resolve::shown(row.view(), row.observation().as_ref(), &ctx),
        };
        choices.push(Choice {
            shown,
            field_rank: row.field_rank,
            status: state.status,
            demoted: state.demoted,
            current: Some(row.id) == current_selected || Some(row.id) == current_notice,
            unconfirmed_hard: state.first_hard_failure_at.is_some()
                && state.status != Status::Broken,
        });
    }
    let (best, notice) = resolve::select(&choices);
    let pending = rows
        .iter()
        .zip(&choices)
        .any(|(r, c)| c.status == Status::Unchecked && r.field != ONE_HOP_FIELD);
    // One unconfirmed failure is not yet "nothing usable" (code review M3).
    let unconfirmed = choices.iter().any(|c| c.unconfirmed_hard);
    let state = if pending {
        "pending"
    } else if best.is_some() {
        "selected"
    } else if unconfirmed {
        "pending"
    } else {
        "none"
    };
    for (row, choice) in rows.iter().zip(&choices) {
        let (kind, sub, access) = match &choice.shown {
            Some(s) => (
                Some(s.kind.as_str()),
                Some(s.sub.as_str()),
                s.access.as_str(),
            ),
            None => (None, None, "direct"),
        };
        if row.kind.as_deref() != kind || row.sub.as_deref() != sub || row.access != access {
            sqlx::query(
                "UPDATE program_link_candidates SET kind=$2,rank=$3,sub=$4,access=$5,updated_at=now() WHERE id=$1",
            )
            .bind(row.id)
            .bind(kind)
            .bind(choice.shown.as_ref().map(|s| s.kind.rank() as i16))
            .bind(sub.map(|s| s.chars().take(80).collect::<String>()))
            .bind(access)
            .execute(&mut *tx)
            .await?;
        }
    }
    let selected_id = best.map(|i| rows[i].id);
    let notice_id = notice.map(|i| rows[i].id);
    sqlx::query(
        "INSERT INTO program_link_selections(program_id,input_hash,state,selected_id,notice_id,selected_at)
         VALUES($1,'',$2,$3,$4,now())
         ON CONFLICT(program_id) DO UPDATE SET state=excluded.state,selected_id=excluded.selected_id,
           notice_id=excluded.notice_id,reselect_due=false,
           selected_at=CASE WHEN program_link_selections.selected_id IS DISTINCT FROM excluded.selected_id
                            THEN now() ELSE program_link_selections.selected_at END",
    )
    .bind(program)
    .bind(state)
    .bind(selected_id)
    .bind(notice_id)
    .execute(&mut *tx)
    .await?;
    // A newly selected link is checked on the selected cadence from now.
    let opens_soon = opens_soon(&mut tx, program).await?;
    for i in [best, notice].into_iter().flatten() {
        let row = &rows[i];
        if let Some(at) = resolve::next_check(&row.state(), true, opens_soon, Utc::now()) {
            sqlx::query(
                "UPDATE program_link_candidates SET next_check_at=LEAST(next_check_at,$2) WHERE id=$1",
            )
            .bind(row.id)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
    }
    // Only a notice page (as shown now, not as stored before this pass).
    let notice_row = notice
        .filter(|i| {
            choices[*i]
                .shown
                .as_ref()
                .is_some_and(|s| s.kind == classify::Kind::Notice)
        })
        .map(|i| &rows[i]);
    let changed = sync_one_hop(&mut tx, program, &rows, notice_row, &nav).await?;
    tx.commit().await?;
    Ok(changed)
}

/// Design 5.8: application links on the programme's notice page become
/// candidates (checked like any other before they are shown).
async fn sync_one_hop(
    tx: &mut PgConnection,
    program: Uuid,
    rows: &[Row],
    notice: Option<&Row>,
    nav: &HashMap<(String, String), i64>,
) -> Result<bool, sqlx::Error> {
    let mut targets: Vec<String> = Vec::new();
    let mut source: Option<&Row> = None;
    // Only a page that stayed on the notice's own site (security review M1).
    let same_site = |a: &str, b: &str| {
        let site = |u: &str| mazimi_core::source_policy::registrable_domain(&classify::host_of(u));
        site(a).is_some() && site(a) == site(b)
    };
    if let Some(row) = notice.filter(|r| r.field != ONE_HOP_FIELD) {
        if let (Some(page), Some(final_url)) = (&row.page, &row.final_url) {
            if !same_site(&row.url, final_url) {
                return clear_one_hop(tx, program, rows).await;
            }
            source = Some(row);
            for (text, href) in &page.0.apply_links {
                let navigation = nav
                    .get(&(row.host.clone(), href.clone()))
                    .is_some_and(|n| *n >= 3);
                if !classify::is_promotable_hop_text(text) || navigation {
                    continue;
                }
                if let Some(target) = candidates::one_hop_target(final_url, href) {
                    if !targets.contains(&target)
                        && !rows
                            .iter()
                            .any(|r| r.url == target && r.field != ONE_HOP_FIELD)
                    {
                        targets.push(target);
                    }
                }
                if targets.len() == MAX_ONE_HOP {
                    break;
                }
            }
        }
    }
    let existing: HashSet<(&str, Option<&str>)> = rows
        .iter()
        .filter(|r| r.field == ONE_HOP_FIELD)
        .map(|r| (r.url.as_str(), r.source_url.as_deref()))
        .collect();
    let source_url = source.map(|r| r.url.as_str());
    let wanted: HashSet<(&str, Option<&str>)> =
        targets.iter().map(|t| (t.as_str(), source_url)).collect();
    if existing == wanted {
        return Ok(false);
    }
    sqlx::query(
        "DELETE FROM program_link_candidates WHERE program_id=$1 AND field=$2
           AND NOT (url=ANY($3) AND source_url IS NOT DISTINCT FROM $4)",
    )
    .bind(program)
    .bind(ONE_HOP_FIELD)
    .bind(&targets)
    .bind(source_url)
    .execute(&mut *tx)
    .await?;
    // A one-hop link is vouched for exactly as its notice page is.
    let (origin, free_text) = source
        .map(|r| (r.origin.as_str(), r.free_text))
        .unwrap_or(("agency", false));
    for (i, target) in targets.iter().enumerate() {
        sqlx::query(
            "INSERT INTO program_link_candidates(program_id,field,field_rank,url,host,origin,free_text,source_url,check_status,next_check_at)
             VALUES($1,$2,$3,$4,$5,$7,$8,$6,'unchecked',now())
             ON CONFLICT(program_id,field,url) DO UPDATE SET source_url=excluded.source_url,field_rank=excluded.field_rank",
        )
        .bind(program)
        .bind(ONE_HOP_FIELD)
        .bind(ONE_HOP_RANK + i as i16)
        .bind(target)
        .bind(classify::host_of(target))
        .bind(source_url)
        .bind(origin)
        .bind(free_text)
        .execute(&mut *tx)
        .await?;
    }
    Ok(true)
}

async fn clear_one_hop(
    tx: &mut PgConnection,
    program: Uuid,
    rows: &[Row],
) -> Result<bool, sqlx::Error> {
    if !rows.iter().any(|r| r.field == ONE_HOP_FIELD) {
        return Ok(false);
    }
    sqlx::query("DELETE FROM program_link_candidates WHERE program_id=$1 AND field=$2")
        .bind(program)
        .bind(ONE_HOP_FIELD)
        .execute(&mut *tx)
        .await?;
    Ok(true)
}

async fn opens_soon(pool: &mut PgConnection, program: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(application_start_at > now() AND application_start_at <= now()+interval '14 days', false)
         FROM programs WHERE id=$1",
    )
    .bind(program)
    .fetch_optional(pool)
    .await?
    .unwrap_or(false))
}

/// Due URLs of current programmes, oldest first, with their request host.
/// A selected link of a programme whose window opens within 14 days is due
/// daily even if its stored date is later (code review L1).
pub async fn claim(pool: &PgPool, limit: i64) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT c.url, min(c.host) FROM program_link_candidates c
         JOIN current_programs p ON p.id=c.program_id
         LEFT JOIN program_link_selections s ON s.program_id=c.program_id
         WHERE c.check_status<>'skipped'
           AND (c.next_check_at<=now()
                OR (c.id IN (s.selected_id, s.notice_id)
                    AND p.application_start_at > now() AND p.application_start_at <= now()+interval '14 days'
                    AND (c.checked_at IS NULL OR c.checked_at < now()-interval '1 day')))
         GROUP BY c.url ORDER BY min(c.next_check_at) NULLS FIRST, c.url LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(FromRow)]
struct CheckedRow {
    #[sqlx(flatten)]
    row: Row,
    selected: bool,
    opens_soon: bool,
}

/// Programmes whose re-selection a stopped run left undone.
pub async fn reselect_due(pool: &PgPool, limit: i64) -> anyhow::Result<usize> {
    let due: Vec<Uuid> = sqlx::query_scalar(
        "SELECT program_id FROM program_link_selections WHERE reselect_due ORDER BY program_id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for program in &due {
        // One programme that fails every time must not wedge the job.
        match reselect(pool, *program).await {
            Ok(()) => done += 1,
            Err(error) => {
                tracing::warn!(program_id = %program, error = %crate::error_safety::persisted_error(&error), "link re-selection failed")
            }
        }
    }
    Ok(done)
}

/// A URL whose result could not be recorded waits an hour instead of
/// heading the queue again (code review H2).
pub async fn postpone(pool: &PgPool, url: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE program_link_candidates SET next_check_at=now()+interval '1 hour' WHERE url=$1 AND check_status<>'skipped'")
        .bind(url)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ApplySummary {
    pub passed: usize,
    pub blocked: usize,
    pub soft: usize,
    pub hard: usize,
    pub newly_broken: usize,
    pub restored: usize,
}

/// Records one check of `url` on every candidate row with that URL and
/// re-selects the programmes concerned.
pub async fn apply(
    pool: &PgPool,
    url: &str,
    observation: &Observation,
    summary: &mut ApplySummary,
) -> anyhow::Result<()> {
    let rows: Vec<CheckedRow> = sqlx::query_as(
        "SELECT c.id,c.program_id,c.field,c.field_rank,c.url,c.host,c.origin,c.free_text,c.source_url,c.kind,c.sub,c.access,
           c.check_status,c.final_url,c.http_status,c.page,c.last_failure_code,c.last_pass_at,c.next_check_at,c.fail_count,
           c.first_fail_at,c.soft_fail_count,c.first_soft_fail_at,c.restore_passes,c.demoted,p.title AS program_title,
           (SELECT n.page->>'title' FROM program_link_candidates n WHERE n.program_id=c.program_id
              AND n.url=c.source_url AND n.field<>'notice_apply_link' LIMIT 1) AS notice_title,
           COALESCE(c.id IN (s.selected_id, s.notice_id), false) AS selected,
           COALESCE(p.application_start_at > now() AND p.application_start_at <= now()+interval '14 days', false) AS opens_soon
         FROM program_link_candidates c JOIN current_programs p ON p.id=c.program_id
         LEFT JOIN program_link_selections s ON s.program_id=c.program_id
         WHERE c.url=$1 AND c.check_status<>'skipped'",
    )
    .bind(url)
    .fetch_all(pool)
    .await?;
    let programs: Vec<Uuid> = rows
        .iter()
        .map(|r| r.row.program_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    // Recorded first: if this run stops before re-selecting, the next run does.
    sqlx::query("UPDATE program_link_selections SET reselect_due=true WHERE program_id=ANY($1)")
        .bind(&programs)
        .execute(pool)
        .await?;
    let now = Utc::now();
    for checked in &rows {
        let row = &checked.row;
        let live = resolve::liveness(row.view(), observation);
        let before = row.state();
        let after = resolve::observe(&before, &live, now);
        match &live {
            Liveness::Pass => summary.passed += 1,
            Liveness::Blocked => summary.blocked += 1,
            Liveness::Soft(_) => summary.soft += 1,
            Liveness::Hard(_) => summary.hard += 1,
        }
        if after.status == Status::Broken && before.status != Status::Broken {
            summary.newly_broken += 1;
        }
        if before.status == Status::Broken && after.status == Status::Ok {
            summary.restored += 1;
        }
        let next = resolve::next_check(&after, checked.selected, checked.opens_soon, now);
        let failure = match &live {
            Liveness::Soft(code) | Liveness::Hard(code) => {
                Some(code.chars().take(80).collect::<String>())
            }
            _ => None,
        };
        // Only a passing check replaces the observation the kind rests on.
        let pass = match (&live, observation) {
            (
                Liveness::Pass,
                Observation::Fetched {
                    status,
                    final_url,
                    page,
                },
            ) => Some((
                *status as i16,
                final_url.chars().take(2048).collect::<String>(),
                page.clone(),
            )),
            _ => None,
        };
        let hash = pass
            .as_ref()
            .and_then(|(_, _, page)| page.as_ref())
            .map(|p| {
                mazimi_core::application_resources::digest(
                    serde_json::to_string(p).unwrap_or_default().as_bytes(),
                )
            });
        sqlx::query(
            "UPDATE program_link_candidates SET check_status=$2,fail_count=$3,first_fail_at=$4,soft_fail_count=$5,
               first_soft_fail_at=$6,restore_passes=$7,demoted=$8,checked_at=$9,next_check_at=$10,
               last_failure_code=CASE WHEN $11::text IS NULL AND $2 IN ('ok','blocked') THEN NULL ELSE COALESCE($11,last_failure_code) END,
               http_status=CASE WHEN $12 THEN $13 ELSE http_status END,
               final_url=CASE WHEN $12 THEN $14 ELSE final_url END,
               page=CASE WHEN $12 THEN $15 ELSE page END,
               content_hash=CASE WHEN $12 THEN $16 ELSE content_hash END,
               last_pass_at=CASE WHEN $12 THEN $9 ELSE last_pass_at END,
               updated_at=now()
             WHERE id=$1",
        )
        .bind(row.id)
        .bind(after.status.as_str())
        .bind(after.hard_failures)
        .bind(after.first_hard_failure_at)
        .bind(after.soft_failures)
        .bind(after.first_soft_failure_at)
        .bind(after.restore_passes)
        .bind(after.demoted)
        .bind(now)
        .bind(next)
        .bind(failure)
        .bind(pass.is_some())
        .bind(pass.as_ref().map(|p| p.0))
        .bind(pass.as_ref().map(|p| p.1.clone()))
        .bind(pass.as_ref().and_then(|p| p.2.clone()).map(Json))
        .bind(hash)
        .execute(pool)
        .await?;
    }
    for program in programs {
        reselect(pool, program).await?;
    }
    Ok(())
}
