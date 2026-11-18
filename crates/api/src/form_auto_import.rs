//! Automatic import of reviewed form templates (서류 작성 도우미 auto templates).
//!
//! Two directories: the run dir (written by the sandboxed steps that parse agency files,
//! so untrusted) and the state dir (keys, the deciding verdict, the worker's candidate
//! rows, the converter output hashes and verdicts.py's manifest; no parsing step can write
//! it). Everything that decides *what* is published and *where* comes from the state dir;
//! the run dir only supplies the template and PDF bytes, which must hash to the state's
//! records.
//!
//! There is no human approval step: a template that passed every machine gate of
//! `scripts/forms/auto` and whose deciding reviewer returned zero findings is
//! published here, with that review recorded as the verifier. Before it writes,
//! this module re-checks the evidence itself instead of trusting the manifest:
//!   - the deciding verdict file echoes the packet nonce, answers `pass` with no
//!     findings for this template, and hashes to what the manifest recorded;
//!   - the template file hashes to what was reviewed and validates;
//!   - the official bytes the boxes sit on equal the hash the worker verified on
//!     the resource row (`expected_content_hash` binding), checked again by the
//!     same service code the admin API uses;
//!   - the reviewer is the independent reviewer, or the local judge only when
//!     `FORM_REVIEW_JUDGE=judge` (verdicts.py admits it only after its gate);
//!   - the resource and programme come from the worker's candidate rows, the converted
//!     PDF hash from the host's record of the converter output;
//!   - a template a person made (updated_by is not the automatic actor) is never
//!     overwritten, and bytes an operator rolled back are never imported again.
//!
//! Every gate and the sensitive-field rules stay where they are (gates.py,
//! fill.cjs, `Template::validate`). Each import, refusal and rollback writes an
//! append-only `form_auto_imports` row: reviewer, verdict sha, template sha.
//!
//! Off unless `FORM_AUTO_IMPORT_ENABLED=true`. Kill switch: while the file at
//! `FORM_AUTO_KILL_FILE` exists nothing is imported. Rollback unpublishes one
//! template (its boxes go offline; the app falls back to the form's own fields).
use crate::services::{
    application_resources::{self, Review},
    form_templates::{self, DerivedUpload, Put},
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use mazimi_core::{
    application_resources::{digest, fingerprint, Resource},
    form_templates::Template,
    models::Program,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const MAX_JSON: u64 = 4 * 1024 * 1024;
const MAX_PDF: u64 = 8 * 1024 * 1024;
/// Persistent storage, so the kill switch survives a reboot (scripts/forms/server/form-auto.sh).
pub const DEFAULT_KILL_FILE: &str = "/var/lib/mazimi/form-auto-control/stop";

#[derive(Debug, Clone)]
pub struct Config {
    /// The user id recorded as reviewer/updater of automatic imports.
    pub actor: Uuid,
    pub kill_file: PathBuf,
    /// The local judge may be the deciding reviewer (FORM_REVIEW_JUDGE=judge).
    pub allow_judge: bool,
}

impl Config {
    /// `Ok(None)` when automatic import is off (the default).
    pub fn from_env() -> Result<Option<Self>> {
        if std::env::var("FORM_AUTO_IMPORT_ENABLED").unwrap_or_default() != "true" {
            return Ok(None);
        }
        let actor = std::env::var("FORM_AUTO_ACTOR_USER_ID")
            .context("FORM_AUTO_ACTOR_USER_ID is required when FORM_AUTO_IMPORT_ENABLED=true")?;
        let actor =
            Uuid::parse_str(actor.trim()).context("FORM_AUTO_ACTOR_USER_ID is not a UUID")?;
        ensure!(!actor.is_nil(), "FORM_AUTO_ACTOR_USER_ID must not be nil");
        let kill_file = PathBuf::from(
            std::env::var("FORM_AUTO_KILL_FILE").unwrap_or_else(|_| DEFAULT_KILL_FILE.into()),
        );
        ensure!(
            kill_file.is_absolute(),
            "FORM_AUTO_KILL_FILE must be absolute"
        );
        let allow_judge = match std::env::var("FORM_REVIEW_JUDGE")
            .unwrap_or_default()
            .as_str()
        {
            "" | "shadow" | "off" => false,
            "judge" => true,
            _ => bail!("FORM_REVIEW_JUDGE must be off, shadow or judge"),
        };
        Ok(Some(Self {
            actor,
            kill_file,
            allow_judge,
        }))
    }

    /// The settings a dry run needs (`form-auto-import check`): the same as an import, without
    /// FORM_AUTO_IMPORT_ENABLED. A dry run writes nothing that stays.
    pub fn for_dry_run() -> Result<Self> {
        let actor = std::env::var("FORM_AUTO_ACTOR_USER_ID")
            .context("FORM_AUTO_ACTOR_USER_ID is required for a dry run")?;
        let actor =
            Uuid::parse_str(actor.trim()).context("FORM_AUTO_ACTOR_USER_ID is not a UUID")?;
        ensure!(!actor.is_nil(), "FORM_AUTO_ACTOR_USER_ID must not be nil");
        let kill_file = PathBuf::from(
            std::env::var("FORM_AUTO_KILL_FILE").unwrap_or_else(|_| DEFAULT_KILL_FILE.into()),
        );
        let allow_judge = std::env::var("FORM_REVIEW_JUDGE").unwrap_or_default() == "judge";
        Ok(Self {
            actor,
            kill_file,
            allow_judge,
        })
    }

    /// True while the kill file exists (any kind of entry, even a dangling link), and also on
    /// any error other than "not found" (a permission error must not read as "not killed").
    pub fn killed(&self) -> bool {
        match std::fs::symlink_metadata(&self.kill_file) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    forms: Vec<Entry>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Entry {
    pub slug: String,
    pub sha256: String,
    pub format: String,
    pub template: String,
    #[serde(default)]
    pub derived_pdf: Option<DerivedPdf>,
    pub review: Evidence,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DerivedPdf {
    pub sha256: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Evidence {
    pub reviewer: String,
    pub verdict_file: String,
    pub verdict_sha256: Option<String>,
    pub template_sha256: Option<String>,
    pub run: String,
}

#[derive(Debug, Deserialize)]
struct Source {
    slug: String,
    resource_id: Uuid,
}

/// A row of the worker's candidate query (state dir), the only source of resource and programme.
#[derive(Debug, Deserialize)]
struct Candidate {
    id: Uuid,
    program_id: Uuid,
    content_hash: Option<String>,
}

/// What the importer did for one form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Outcome {
    Imported {
        revision: i64,
        post_check_pending: bool,
    },
    UpToDate,
    Refused(String),
    Killed,
    /// Dry run: every check passed; what publishing would do.
    WouldImport(String),
}

/// A checked, ready-to-publish form.
#[derive(Debug)]
pub struct Checked {
    pub entry: Entry,
    pub template: Template,
    pub template_sha256: String,
    pub verdict_sha256: String,
    pub official_sha256: String,
    pub pdf: Option<Vec<u8>>,
    pub resource_id: Uuid,
    pub program_id: Uuid,
}

fn slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn sha_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads a regular file inside `root` (no symlink, no `..`), at most `cap` bytes.
pub fn read_inside(root: &Path, rel: &Path, cap: u64) -> Result<Vec<u8>> {
    ensure!(
        rel.components().all(|c| matches!(c, Component::Normal(_))),
        "path {} leaves the run directory",
        rel.display()
    );
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        let meta = std::fs::symlink_metadata(&cur)
            .with_context(|| format!("{} missing", rel.display()))?;
        ensure!(
            !meta.file_type().is_symlink(),
            "{} is a symlink",
            rel.display()
        );
    }
    let meta = std::fs::symlink_metadata(&cur)?;
    ensure!(meta.is_file(), "{} is not a regular file", rel.display());
    ensure!(
        meta.len() > 0 && meta.len() <= cap,
        "{} size {} out of bounds",
        rel.display(),
        meta.len()
    );
    Ok(std::fs::read(&cur)?)
}

fn json_inside<T: for<'de> Deserialize<'de>>(root: &Path, rel: &str) -> Result<T> {
    let bytes = read_inside(root, Path::new(rel), MAX_JSON)?;
    serde_json::from_slice(&bytes).with_context(|| format!("{rel} is not valid JSON"))
}

/// The trusted inputs of one run, read from the state dir.
struct State {
    key: serde_json::Map<String, Value>,
    nonce: String,
    candidates: Vec<Candidate>,
    /// "<slug>.pdf" -> sha256 of what the converter wrote, recorded by the host.
    derived: std::collections::HashMap<String, String>,
}

/// Re-derives everything the import relies on. Pure file checks: no database, no network.
/// A refusal names the failed check.
pub fn check(
    run: &Path,
    state: &Path,
    cfg: &Config,
) -> Result<Vec<std::result::Result<Checked, (String, String)>>> {
    let manifest: Manifest = json_inside(state, "out/import-manifest.json")?;
    if manifest.forms.is_empty() {
        return Ok(Vec::new());
    }
    let key: serde_json::Map<String, Value> = json_inside(state, "keys/batch.key.json")?;
    let nonce = key
        .get("_nonce")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("packet key has no nonce"))?
        .to_string();
    let candidates: Vec<Candidate> = json_inside(state, "cand/candidates.raw.json")?;
    // Absent when the run had no HWP/HWPX; any other failure to read it is an error, never an
    // empty record (which would refuse every converted form for the wrong reason).
    let derived = match std::fs::symlink_metadata(state.join("derived.sha256")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        _ => {
            let bytes = match std::fs::metadata(state.join("derived.sha256")) {
                Ok(m) if m.len() == 0 => Vec::new(),
                _ => read_inside(state, Path::new("derived.sha256"), MAX_JSON)
                    .context("state derived.sha256 unreadable")?,
            };
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    Some((it.next()?.to_string(), it.next()?.to_string()))
                })
                .collect()
        }
    };
    let st = State {
        key,
        nonce,
        candidates,
        derived,
    };
    let sources: Vec<Source> = json_inside(run, "sources.json")?;
    let mut out = Vec::new();
    for entry in manifest.forms {
        let slug = if slug_ok(&entry.slug) {
            entry.slug.clone()
        } else {
            "invalid-slug".into()
        };
        out.push(
            check_one(run, state, cfg, &st, &sources, entry).map_err(|e| (slug, format!("{e:#}"))),
        );
    }
    Ok(out)
}

fn check_one(
    run: &Path,
    state: &Path,
    cfg: &Config,
    st: &State,
    sources: &[Source],
    entry: Entry,
) -> Result<Checked> {
    let (key, nonce) = (&st.key, st.nonce.as_str());
    ensure!(slug_ok(&entry.slug), "bad slug");
    ensure!(sha_ok(&entry.sha256), "bad official sha256");
    let verdict_rel = match entry.review.reviewer.as_str() {
        "claude-reviewer" => "verdict/batch.verdict.json",
        "judge" if cfg.allow_judge => "judge/batch.judge.json",
        "judge" => {
            bail!("the local judge is not the deciding reviewer (FORM_REVIEW_JUDGE is not judge)")
        }
        other => bail!("unknown reviewer {other}"),
    };
    ensure!(
        Path::new(&entry.review.verdict_file).file_name() == Path::new(verdict_rel).file_name(),
        "manifest names another verdict file"
    );
    let run_name = run.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    ensure!(entry.review.run == run_name, "evidence is from another run");
    let verdict_bytes = read_inside(state, Path::new(verdict_rel), MAX_JSON)?;
    let verdict_sha256 = digest(&verdict_bytes);
    ensure!(
        entry.review.verdict_sha256.as_deref() == Some(verdict_sha256.as_str()),
        "verdict file changed after verdicts.py"
    );
    let verdicts: serde_json::Map<String, Value> =
        serde_json::from_slice(&verdict_bytes).context("verdict file is not JSON")?;
    ensure!(
        verdicts.get("nonce").and_then(Value::as_str) == Some(nonce),
        "verdict does not echo the packet nonce"
    );
    let letter = key
        .iter()
        .find(|(k, v)| {
            !k.starts_with('_')
                && v.get("slug").and_then(Value::as_str) == Some(entry.slug.as_str())
        })
        .map(|(k, _)| k.clone())
        .ok_or_else(|| anyhow!("template is not in the review packet"))?;
    let v = verdicts
        .get(&letter)
        .ok_or_else(|| anyhow!("no verdict for {letter}"))?;
    ensure!(
        v.get("verdict").and_then(Value::as_str) == Some("pass")
            && v.get("findings")
                .and_then(Value::as_array)
                .is_some_and(|f| f.is_empty()),
        "reviewer did not pass it with zero findings"
    );
    let rel = format!("out/{}.template.json", entry.slug);
    ensure!(
        entry
            .template
            .ends_with(&format!("{}.template.json", entry.slug)),
        "manifest template path does not match the slug"
    );
    let tbytes = read_inside(run, Path::new(&rel), MAX_JSON)?;
    let template_sha256 = digest(&tbytes);
    let reviewed = key[&letter].get("template_sha256").and_then(Value::as_str);
    ensure!(
        reviewed == Some(template_sha256.as_str()),
        "template changed after review"
    );
    ensure!(
        entry.review.template_sha256.as_deref() == Some(template_sha256.as_str()),
        "template changed after verdicts.py"
    );
    let template: Template = serde_json::from_slice(&tbytes).context("template JSON")?;
    template
        .validate()
        .map_err(|e| anyhow!("template invalid: {e}"))?;
    let derived = matches!(entry.format.as_str(), "hwp" | "hwpx");
    let official = match (&template.derived, derived) {
        (Some(d), true) => d.source_sha256.clone(),
        (None, false) => template.content_hash.clone(),
        _ => bail!("format and derived provenance disagree"),
    };
    ensure!(
        official == entry.sha256,
        "template is not bound to the reviewed official file"
    );
    let pdf = if derived {
        let want = entry.derived_pdf.as_ref().map(|d| d.sha256.as_str());
        ensure!(
            want == Some(template.content_hash.as_str()),
            "derived PDF hash disagrees with the template"
        );
        let bytes = read_inside(
            run,
            Path::new(&format!("derived/{}.pdf", entry.slug)),
            MAX_PDF,
        )?;
        ensure!(
            digest(&bytes) == template.content_hash,
            "converted PDF on disk is not the one the boxes sit on"
        );
        // The bytes the converter wrote, as the host hashed them before any parsing step ran.
        ensure!(
            st.derived.get(&format!("{}.pdf", entry.slug)) == Some(&template.content_hash),
            "converted PDF is not the converter's output for this file"
        );
        Some(bytes)
    } else {
        None
    };
    let src = sources
        .iter()
        .find(|s| s.slug == entry.slug)
        .ok_or_else(|| anyhow!("no source row"))?;
    let cand = st
        .candidates
        .iter()
        .find(|c| c.id == src.resource_id && c.content_hash.as_deref() == Some(official.as_str()))
        .ok_or_else(|| {
            anyhow!("resource is not a candidate row of this run for these official bytes")
        })?;
    Ok(Checked {
        template_sha256,
        verdict_sha256,
        official_sha256: official,
        pdf,
        resource_id: cand.id,
        program_id: cand.program_id,
        template,
        entry,
    })
}

/// One `form_auto_imports` row (append-only).
#[derive(Debug, Default, Clone, Copy)]
struct Row<'a> {
    action: &'a str,
    resource: Option<Uuid>,
    program: Option<Uuid>,
    slug: &'a str,
    reviewer: &'a str,
    verdict_sha256: Option<&'a str>,
    template_sha256: Option<&'a str>,
    official_sha256: Option<&'a str>,
    run: &'a str,
    revision: Option<i64>,
    detail: &'a str,
    post_check_pending: bool,
    attempt: Option<i64>,
    checks_import: Option<i64>,
    post_check: Option<&'a str>,
}

async fn audit(pool: &PgPool, actor: Uuid, r: Row<'_>) -> Result<i64> {
    let slug = if slug_ok(r.slug) || r.slug == "-" {
        r.slug
    } else {
        "invalid-slug"
    };
    let id = sqlx::query_scalar(
        "INSERT INTO form_auto_imports(action,resource_id,program_id,slug,reviewer,verdict_sha256,template_sha256,official_sha256,run,revision,actor,detail,post_check_pending,checks_import,post_check,attempt)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) RETURNING id",
    )
    .bind(r.action)
    .bind(r.resource)
    .bind(r.program)
    .bind(slug)
    .bind(r.reviewer)
    .bind(r.verdict_sha256)
    .bind(r.template_sha256)
    .bind(r.official_sha256)
    .bind(r.run.chars().take(80).collect::<String>())
    .bind(r.revision)
    .bind(actor)
    .bind(r.detail.chars().take(500).collect::<String>())
    .bind(r.post_check_pending)
    .bind(r.checks_import)
    .bind(r.post_check)
    .bind(r.attempt)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Automatic imports a person (Claude, from the review packet) checks after publishing. Once
/// this many post-checks came back ok, new imports are no longer flagged.
pub const POST_CHECKS: i64 = 20;
/// While flagging, at most this many imports may wait for their post-check; further imports
/// are refused ('post-check backlog') until the checks catch up.
pub const MAX_PENDING: usize = 3;

async fn clean_post_checks(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM form_auto_imports WHERE action='post_check' AND post_check='ok'",
    )
    .fetch_one(pool)
    .await?)
}

/// "<day>/<run>" when the run dir sits in a day directory (the server layout), else the run name.
fn run_label(run: &Path) -> String {
    let name = run.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    match run
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        Some(day) if day.len() == 8 && day.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{day}/{name}")
        }
        _ => name.to_string(),
    }
}

/// Publishes every checked form of a run. Stops at the kill switch.
///
/// Per form: the backlog gate (while flagging, at most MAX_PENDING imports wait for their
/// post-check), then a 'publishing' audit row BEFORE anything is published (it carries the
/// post-check flag, so a crash after publishing cannot lose the check), then publish, then the
/// outcome row bound to that attempt.
pub async fn apply(
    pool: &PgPool,
    cfg: &Config,
    run: &Path,
    state: &Path,
) -> Result<Vec<(String, Outcome)>> {
    let label = run_label(run);
    let mut out = Vec::new();
    for item in check(run, state, cfg)? {
        if cfg.killed() {
            out.push((
                item.as_ref()
                    .map(|c| c.entry.slug.clone())
                    .unwrap_or_else(|e| e.0.clone()),
                Outcome::Killed,
            ));
            continue;
        }
        match item {
            Err((slug, why)) => {
                let row = Row {
                    action: "refused",
                    slug: &slug,
                    reviewer: "-",
                    run: &label,
                    detail: &why,
                    ..Default::default()
                };
                audit(pool, cfg.actor, row).await?;
                out.push((slug, Outcome::Refused(why)));
            }
            Ok(c) => {
                let slug = c.entry.slug.clone();
                let base = Row {
                    resource: Some(c.resource_id),
                    program: Some(c.program_id),
                    slug: &slug,
                    reviewer: &c.entry.review.reviewer,
                    verdict_sha256: Some(&c.verdict_sha256),
                    template_sha256: Some(&c.template_sha256),
                    official_sha256: Some(&c.official_sha256),
                    run: &label,
                    ..Default::default()
                };
                let flag = clean_post_checks(pool).await? < POST_CHECKS;
                if flag {
                    let backlog = pending(pool).await?.0.len();
                    if backlog >= MAX_PENDING {
                        let why = format!("post-check backlog: {backlog} imports wait for a post-check (at most {MAX_PENDING} until {POST_CHECKS} came back ok)");
                        audit(
                            pool,
                            cfg.actor,
                            Row {
                                action: "refused",
                                detail: &why,
                                ..base
                            },
                        )
                        .await?;
                        out.push((slug, Outcome::Refused(why)));
                        continue;
                    }
                }
                let attempt = audit(
                    pool,
                    cfg.actor,
                    Row {
                        action: "publishing",
                        post_check_pending: flag,
                        ..base
                    },
                )
                .await?;
                let outcome = match publish(pool, cfg, &c).await {
                    Ok(Outcome::Imported { revision, .. }) => Outcome::Imported {
                        revision,
                        post_check_pending: flag,
                    },
                    Ok(o) => o,
                    Err(e) => Outcome::Refused(format!("{e:#}")),
                };
                let (action, revision, detail) = match &outcome {
                    Outcome::Imported { revision, .. } => {
                        ("import", Some(*revision), String::new())
                    }
                    Outcome::UpToDate => ("up_to_date", None, String::new()),
                    Outcome::Refused(why) => ("refused", None, why.clone()),
                    Outcome::Killed | Outcome::WouldImport(_) => {
                        ("refused", None, "kill switch".into())
                    }
                };
                audit(
                    pool,
                    cfg.actor,
                    Row {
                        action,
                        revision,
                        detail: &detail,
                        attempt: Some(attempt),
                        ..base
                    },
                )
                .await?;
                out.push((slug, outcome));
            }
        }
    }
    Ok(out)
}

/// Everything publish() checks before it writes, read-only; shared by publish and the dry run.
/// Everything a publish checks before it writes, read-only: the one predicate apply() and the
/// dry run share. It includes every check the final verification makes on the row (the
/// service's verification_refusal) and the template writes' own scope rule (no financial
/// products), so a dry run's "would import" is what apply() will do.
async fn preconditions(pool: &PgPool, cfg: &Config, c: &Checked) -> Result<Resource> {
    let row: Option<Resource> =
        sqlx::query_as("SELECT * FROM program_application_resources WHERE id=$1")
            .bind(c.resource_id)
            .fetch_optional(pool)
            .await?;
    let row = row.ok_or_else(|| anyhow!("resource row gone"))?;
    ensure!(
        row.kind == "template" && row.program_id == c.program_id,
        "resource is not this programme's template"
    );
    ensure!(
        matches!(row.status.as_str(), "candidate" | "verified"),
        "resource is {}",
        row.status
    );
    // expected_content_hash: the worker-verified bytes must still be the reviewed ones.
    ensure!(
        row.content_hash.as_deref() == Some(c.official_sha256.as_str()),
        "official file changed since the review"
    );
    ensure!(
        row.file_format.as_deref() == Some(c.entry.format.as_str()),
        "official file format changed"
    );
    let program: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
        .bind(c.program_id)
        .fetch_one(pool)
        .await?;
    ensure!(
        program.program_type != "financial_product",
        "financial-product applications are out of the form assistant's scope"
    );
    let rolled_back: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM form_auto_imports WHERE action='rollback' AND resource_id=$1 AND official_sha256=$2)",
    )
    .bind(c.resource_id)
    .bind(&c.official_sha256)
    .fetch_one(pool)
    .await?;
    ensure!(
        !rolled_back,
        "an operator rolled these bytes back: not imported again"
    );
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT updated_by FROM application_form_templates WHERE resource_id=$1",
    )
    .bind(c.resource_id)
    .fetch_optional(pool)
    .await?;
    ensure!(
        owner.is_none_or(|o| o == cfg.actor),
        "a person made the current template: not overwritten"
    );
    if row.status != "verified" {
        let mut conn = pool.acquire().await?;
        if let Some(refusal) = application_resources::verification_refusal(
            &mut conn,
            &program,
            &row,
            chrono::Utc::now(),
        )
        .await?
        {
            bail!("verification would be refused: {refusal}");
        }
    }
    Ok(row)
}

/// Writes the converted PDF and the template first, then verifies the resource last: the
/// verification is the commit point. Until it, nothing is visible (the public routes serve a
/// template only for a published resource), so any refusal before it leaves no verified
/// resource behind.
/// The stored template: revision, content hash, fields, groups, source sha256, converter.
type StoredTemplate = (i64, String, Value, Value, Option<String>, Option<String>);

async fn publish(pool: &PgPool, cfg: &Config, c: &Checked) -> Result<Outcome> {
    let row = preconditions(pool, cfg, c).await?;
    let current: Option<StoredTemplate> = sqlx::query_as(
        "SELECT revision,content_hash,fields,groups,source_sha256,converter FROM application_form_templates WHERE resource_id=$1",
    )
    .bind(c.resource_id)
    .fetch_optional(pool)
    .await?;
    let t = &c.template;
    if let Some(pdf) = &c.pdf {
        let d = t.derived.as_ref().expect("checked: derived template");
        let stored: Option<(String, String)> = sqlx::query_as(
            "SELECT content_hash,converter FROM application_form_derived_pdfs WHERE resource_id=$1",
        )
        .bind(c.resource_id)
        .fetch_optional(pool)
        .await?;
        if stored.as_ref().map(|(h, conv)| (h.as_str(), conv.as_str()))
            != Some((t.content_hash.as_str(), d.converter.as_str()))
        {
            form_templates::admin_put_derived_pdf(
                pool,
                cfg.actor,
                c.resource_id,
                DerivedUpload {
                    source_sha256: d.source_sha256.clone(),
                    converter: d.converter.clone(),
                    expected_content_hash: stored.map(|(h, _)| h),
                },
                pdf,
            )
            .await
            .map_err(|e| anyhow!("converted PDF refused: {e}"))?;
        }
    }
    let same = current
        .as_ref()
        .is_some_and(|(_, hash, fields, groups, src, conv)| {
            hash == &t.content_hash
                && fields == &json!(t.fields)
                && groups == &json!(t.groups)
                && src.as_deref() == t.derived.as_ref().map(|d| d.source_sha256.as_str())
                && conv.as_deref() == t.derived.as_ref().map(|d| d.converter.as_str())
        });
    let revision = if same {
        current.as_ref().map(|c| c.0).unwrap_or_default()
    } else {
        let saved = form_templates::admin_put(
            pool,
            cfg.actor,
            c.resource_id,
            Put {
                template: t.clone(),
                expected_revision: current.as_ref().map(|c| c.0),
            },
        )
        .await
        .map_err(|e| anyhow!("template refused: {e}"))?;
        saved["revision"].as_i64().unwrap_or_default()
    };
    if row.status == "verified" {
        return Ok(if same {
            Outcome::UpToDate
        } else {
            Outcome::Imported {
                revision,
                post_check_pending: false,
            }
        });
    }
    // The commit point: re-read the row (its revision) and verify.
    let row: Resource = sqlx::query_as("SELECT * FROM program_application_resources WHERE id=$1")
        .bind(c.resource_id)
        .fetch_one(pool)
        .await?;
    let program: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
        .bind(c.program_id)
        .fetch_one(pool)
        .await?;
    let note = format!(
        "자동 검토 통과: reviewer={} run={} verdict_sha256={} template_sha256={} (machine gates, slot check, zero findings)",
        c.entry.review.reviewer, c.entry.review.run, c.verdict_sha256, c.template_sha256
    );
    let verified = verify_hook().await;
    let verified = match verified {
        Some(e) => Err(e),
        None => application_resources::review(
            pool,
            cfg.actor,
            c.resource_id,
            Review {
                decision: "verified".into(),
                expected_revision: row.revision,
                program_fingerprint: fingerprint(&program),
                access: Some("direct".into()),
                same_program_confirmed: true,
                current_intake_confirmed: true,
                destination_confirmed: true,
                note,
                validity_hours: None,
            },
        )
        .await
        .map(|_| ()),
    };
    if let Err(e) = verified {
        // Refused at the commit point: take back the template this attempt wrote, so a person's
        // later verification of the resource cannot publish it without a post-check.
        // Only the row this attempt wrote: a person who saved their own template, or verified
        // one, in the same window keeps it (no row deleted is fine).
        if !same {
            if let Err(d) = sqlx::query("DELETE FROM application_form_templates WHERE resource_id=$1 AND updated_by=$2 AND revision=$3")
                .bind(c.resource_id)
                .bind(cfg.actor)
                .bind(revision)
                .execute(pool)
                .await
            {
                bail!("resource verification refused ({e}) and the unpublished template could not be removed: {d}");
            }
        }
        bail!("resource verification refused (its template was removed again): {e}");
    }
    Ok(Outcome::Imported {
        revision,
        post_check_pending: false,
    })
}

#[cfg(test)]
static FORCE_VERIFY_CONFLICT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A test seam: a verification that loses a race (revision or fingerprint) answers Conflict.
async fn verify_hook() -> Option<application_resources::Error> {
    #[cfg(test)]
    if FORCE_VERIFY_CONFLICT.load(std::sync::atomic::Ordering::SeqCst) {
        return Some(application_resources::Error::Conflict);
    }
    None
}

/// The importer against the real database without publishing: every file check, every
/// read-only precondition of publish, the backlog gate, and the audit insert (in a transaction
/// that is rolled back, so the table's constraints, triggers and grants are exercised and
/// nothing stays). The first server run uses this with FORM_AUTO_IMPORT_ENABLED=false.
pub async fn dry_run(
    pool: &PgPool,
    cfg: &Config,
    run: &Path,
    state: &Path,
) -> Result<Vec<(String, Outcome)>> {
    let label = run_label(run);
    let mut out = Vec::new();
    for item in check(run, state, cfg)? {
        let c = match item {
            Err((slug, why)) => {
                out.push((slug, Outcome::Refused(why)));
                continue;
            }
            Ok(c) => c,
        };
        let slug = c.entry.slug.clone();
        let flag = clean_post_checks(pool).await? < POST_CHECKS;
        let backlog = pending(pool).await?.0.len();
        let res = async {
            ensure!(!flag || backlog < MAX_PENDING, "post-check backlog: {backlog} imports wait for a post-check");
            let row = preconditions(pool, cfg, &c).await?;
            let mut tx = pool.begin().await?;
            sqlx::query(
                "INSERT INTO form_auto_imports(action,resource_id,program_id,slug,reviewer,verdict_sha256,template_sha256,official_sha256,run,actor,post_check_pending)
                 VALUES('publishing',$1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            )
            .bind(c.resource_id)
            .bind(c.program_id)
            .bind(&slug)
            .bind(&c.entry.review.reviewer)
            .bind(&c.verdict_sha256)
            .bind(&c.template_sha256)
            .bind(&c.official_sha256)
            .bind(label.chars().take(80).collect::<String>())
            .bind(cfg.actor)
            .bind(flag)
            .execute(&mut *tx)
            .await
            .context("the audit insert failed")?;
            tx.rollback().await?;
            Ok(format!(
                "would {} resource {} and publish {} fields{}",
                if row.status == "verified" { "keep the verified" } else { "verify" },
                c.resource_id,
                c.template.fields.len(),
                if flag { ", flagged for a post-check" } else { "" }
            ))
        }
        .await;
        out.push((
            slug,
            match res {
                Ok(what) => Outcome::WouldImport(what),
                Err(e) => Outcome::Refused(format!("{e:#}")),
            },
        ));
    }
    Ok(out)
}

/// Rolls one automatic import back: its boxes go offline and, when the automatic importer is
/// the one that verified the resource, the resource is un-verified (rejected) so the form is no
/// longer offered as the programme's official form. Then the rollback row is written, which
/// also refuses these bytes from then on. Every step re-checks the actual state, so a retry
/// after any failure finishes the job instead of trusting an earlier row.
/// A manual rollback (`form-auto.sh rollback`) also answers every post-check still pending
/// for the resource, so a rolled-back import no longer holds a backlog slot.
pub async fn rollback(pool: &PgPool, actor: Uuid, resource: Uuid, reason: &str) -> Result<()> {
    undo(pool, actor, resource, reason).await?;
    answer_pending(
        pool,
        actor,
        resource,
        "-",
        &format!("rolled back: {}", reason.trim()),
    )
    .await
}

/// Answers, as rolled back, every pending attempt for `resource`.
async fn answer_pending(
    pool: &PgPool,
    actor: Uuid,
    resource: Uuid,
    reviewer: &str,
    detail: &str,
) -> Result<()> {
    let detail: String = detail.chars().take(500).collect();
    for p in pending(pool)
        .await?
        .0
        .into_iter()
        .filter(|p| p.resource_id == Some(resource))
    {
        let row = Row {
            action: "post_check",
            resource: Some(resource),
            slug: &p.slug,
            reviewer,
            template_sha256: p.template_sha256.as_deref(),
            run: &p.run,
            detail: &detail,
            checks_import: Some(p.id),
            post_check: Some("rollback"),
            ..Default::default()
        };
        audit(pool, actor, row).await?;
    }
    Ok(())
}

/// The rollback steps themselves (see rollback): idempotent, each re-checks the actual state.
async fn undo(pool: &PgPool, actor: Uuid, resource: Uuid, reason: &str) -> Result<()> {
    let reason = reason.trim();
    ensure!(
        (10..=500).contains(&reason.chars().count()),
        "give a 10-500 character reason"
    );
    let row: Option<Resource> =
        sqlx::query_as("SELECT * FROM program_application_resources WHERE id=$1")
            .bind(resource)
            .fetch_optional(pool)
            .await?;
    let row = row.ok_or_else(|| anyhow!("no such resource"))?;
    // 1. Unpublish. Already gone is done.
    match form_templates::admin_delete(pool, actor, resource).await {
        Ok(()) | Err(application_resources::Error::NotFound) => {}
        Err(e) => bail!("unpublish failed (nothing recorded; run the rollback again): {e}"),
    }
    // 2. Un-verify what the automatic importer verified; a person's verification stays.
    let demote = row.status == "verified" && row.reviewed_by == Some(actor);
    if demote {
        let program: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(row.program_id)
            .fetch_one(pool)
            .await?;
        application_resources::review(
            pool,
            actor,
            resource,
            Review {
                decision: "rejected".into(),
                expected_revision: row.revision,
                program_fingerprint: fingerprint(&program),
                access: None,
                same_program_confirmed: false,
                current_intake_confirmed: false,
                destination_confirmed: false,
                note: format!("자동 가져오기 되돌림(rollback): {}", reason.chars().take(400).collect::<String>()),
                validity_hours: None,
            },
        )
        .await
        .map_err(|e| anyhow!("un-verifying the resource failed (the template is offline; run the rollback again): {e}"))?;
    }
    // 3. The actual state, whatever the steps above reported.
    let (published, verified_by_importer): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM application_form_templates WHERE resource_id=$1),
                EXISTS(SELECT 1 FROM program_application_resources WHERE id=$1 AND status='verified' AND reviewed_by=$2)",
    )
    .bind(resource)
    .bind(actor)
    .fetch_one(pool)
    .await?;
    ensure!(!published && !verified_by_importer, "rollback did not take effect (published {published}, verified by the importer {verified_by_importer}): run it again");
    // 4. The record (once per rollback of these bytes since the last attempt).
    let recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM form_auto_imports r WHERE r.action='rollback' AND r.resource_id=$1
                AND r.official_sha256 IS NOT DISTINCT FROM $2
                AND r.id > COALESCE((SELECT max(id) FROM form_auto_imports WHERE action='publishing' AND resource_id=$1), 0))",
    )
    .bind(resource)
    .bind(row.content_hash.as_deref())
    .fetch_one(pool)
    .await?;
    if !recorded {
        let detail = format!(
            "{} | unpublished; resource {}",
            reason,
            if demote {
                "rejected (was verified by the importer)"
            } else {
                "left as it was"
            }
        );
        let r = Row {
            action: "rollback",
            resource: Some(resource),
            program: Some(row.program_id),
            slug: "-",
            reviewer: "-",
            official_sha256: row.content_hash.as_deref(),
            run: "-",
            detail: &detail,
            ..Default::default()
        };
        audit(pool, actor, r).await?;
    }
    Ok(())
}

/// An import attempt still waiting for its post-check.
#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct Pending {
    pub id: i64,
    pub slug: String,
    pub resource_id: Option<Uuid>,
    pub run: String,
    pub reviewer: String,
    pub template_sha256: Option<String>,
    /// The published revision; None when the outcome row is missing (a crash after publishing).
    pub revision: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn pending(pool: &PgPool) -> Result<(Vec<Pending>, i64)> {
    let rows = sqlx::query_as(
        "SELECT p.id, p.slug, p.resource_id, p.run, p.reviewer, p.template_sha256,
                (SELECT o.revision FROM form_auto_imports o WHERE o.attempt=p.id AND o.action='import' LIMIT 1) AS revision,
                p.created_at
           FROM form_auto_imports p
          WHERE p.action='publishing' AND p.post_check_pending
            AND NOT EXISTS (SELECT 1 FROM form_auto_imports o WHERE o.attempt=p.id AND o.action IN ('up_to_date','refused'))
            AND NOT EXISTS (SELECT 1 FROM form_auto_imports c WHERE c.action='post_check' AND c.checks_import=p.id)
          ORDER BY p.id",
    )
    .fetch_all(pool)
    .await?;
    Ok((rows, clean_post_checks(pool).await?))
}

/// What a post-check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCheck {
    Ok,
    Rollback,
}

impl std::str::FromStr for PostCheck {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "ok" => Ok(Self::Ok),
            "rollback" => Ok(Self::Rollback),
            _ => bail!("the post-check answer is ok or rollback"),
        }
    }
}

/// Answers one pending import attempt. `rollback` runs the rollback state machine first
/// (idempotent, re-checks the actual state); the answer is recorded only after it succeeded,
/// so an attempt is never marked checked while its template is still live.
pub async fn post_check(
    pool: &PgPool,
    actor: Uuid,
    attempt: i64,
    answer: PostCheck,
    reason: &str,
) -> Result<()> {
    ensure!(
        (10..=500).contains(&reason.trim().chars().count()),
        "give a 10-500 character reason"
    );
    let (pending, _) = pending(pool).await?;
    let p = pending
        .iter()
        .find(|p| p.id == attempt)
        .ok_or_else(|| anyhow!("import {attempt} is not waiting for a post-check"))?;
    let resource = p
        .resource_id
        .ok_or_else(|| anyhow!("import {attempt} has no resource"))?;
    if answer == PostCheck::Rollback {
        undo(
            pool,
            actor,
            resource,
            &format!(
                "post-check of import {attempt}: {}",
                reason.trim().chars().take(450).collect::<String>()
            ),
        )
        .await?;
    }
    let row = Row {
        action: "post_check",
        resource: Some(resource),
        slug: &p.slug,
        reviewer: "claude-post-check",
        template_sha256: p.template_sha256.as_deref(),
        run: &p.run,
        detail: reason.trim(),
        checks_import: Some(attempt),
        post_check: Some(if answer == PostCheck::Ok {
            "ok"
        } else {
            "rollback"
        }),
        ..Default::default()
    };
    audit(pool, actor, row).await?;
    if answer == PostCheck::Rollback {
        // Other attempts for the same resource were rolled back with it.
        answer_pending(
            pool,
            actor,
            resource,
            "claude-post-check",
            &format!("rolled back with import {attempt}"),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PDF: &[u8] = b"%PDF-1.7\n% blank\n%%EOF\n";
    const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn cfg(dir: &Path) -> Config {
        Config {
            actor: Uuid::from_u128(7),
            kill_file: dir.join("stop"),
            allow_judge: false,
        }
    }

    struct Fixture {
        _d: tempdir::Dir,
        run: PathBuf,
        state: PathBuf,
        tsha: String,
    }

    /// A run dir (untrusted) and its state dir (trusted) with one reviewed template.
    /// `hwp`: an HWP form whose boxes sit on the converted PDF.
    fn fixture(verdict: Value, reviewer: &str, hwp: bool) -> Fixture {
        fixture_for(
            verdict,
            reviewer,
            hwp,
            Uuid::from_u128(1),
            Uuid::from_u128(2),
        )
    }

    /// The run's candidate row is `resource` of `program` (a DB test passes real rows).
    fn fixture_for(
        verdict: Value,
        reviewer: &str,
        hwp: bool,
        resource: Uuid,
        program: Uuid,
    ) -> Fixture {
        let d = tempdir::Dir::new();
        let run = d.path().join("run-120000");
        let state = d.path().join("state");
        for sub in ["out", "review", "derived"] {
            fs::create_dir_all(run.join(sub)).unwrap();
        }
        for sub in ["out", "keys", "verdict", "judge", "cand"] {
            fs::create_dir_all(state.join(sub)).unwrap();
        }
        let pdf_sha = digest(PDF);
        let mut template = json!({"version":1,"content_hash":pdf_sha,"page_count":1,"source":"admin",
            "fields":[{"id":"p1-f01","page":0,"rect":{"x":0.1,"y":0.1,"w":0.3,"h":0.03},"type":"text","label":"성명","mapping":"name","required":true}]});
        let official = if hwp {
            SOURCE.to_string()
        } else {
            pdf_sha.clone()
        };
        if hwp {
            template["derived"] = json!({"source_sha256": SOURCE, "converter": "rhwp 0.8.6 export-pdf svg; image sha256:test"});
            fs::write(run.join("derived/s.pdf"), PDF).unwrap();
            fs::write(state.join("derived.sha256"), format!("s.pdf {pdf_sha}\n")).unwrap();
        }
        let tbytes = serde_json::to_vec(&template).unwrap();
        fs::write(run.join("out/s.template.json"), &tbytes).unwrap();
        let tsha = digest(&tbytes);
        fs::write(
            state.join("keys/batch.key.json"),
            json!({"_nonce":"n0","A":{"slug":"s","template":"x","template_sha256":tsha}})
                .to_string(),
        )
        .unwrap();
        let vrel = if reviewer == "judge" {
            "judge/batch.judge.json"
        } else {
            "verdict/batch.verdict.json"
        };
        let vbytes = verdict.to_string();
        fs::write(state.join(vrel), &vbytes).unwrap();
        fs::write(
            run.join("sources.json"),
            json!([{"slug":"s","resource_id":resource}]).to_string(),
        )
        .unwrap();
        fs::write(
            state.join("cand/candidates.raw.json"),
            json!([{"id":resource,"program_id":program,"content_hash":official}]).to_string(),
        )
        .unwrap();
        let mut entry = json!({"slug":"s","sha256":official,"format":if hwp {"hwp"} else {"pdf"},"template":"s.template.json",
            "review":{"reviewer":reviewer,"verdict_file":format!("/x/{vrel}"),"verdict_sha256":digest(vbytes.as_bytes()),
                      "template_sha256":tsha,"run":"run-120000"}});
        if hwp {
            entry["derived_pdf"] = json!({"sha256": pdf_sha});
        }
        fs::write(
            state.join("out/import-manifest.json"),
            json!({"forms":[entry]}).to_string(),
        )
        .unwrap();
        Fixture {
            _d: d,
            run,
            state,
            tsha,
        }
    }

    fn pass() -> Value {
        json!({"nonce":"n0","A":{"verdict":"pass","findings":[]}})
    }

    fn only(f: &Fixture, c: &Config) -> std::result::Result<Checked, (String, String)> {
        check(&f.run, &f.state, c)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn why(f: &Fixture) -> String {
        only(f, &cfg(&f.state)).unwrap_err().1
    }

    #[test]
    fn reviewed_pass_is_ready_to_publish() {
        for hwp in [false, true] {
            let f = fixture(pass(), "claude-reviewer", hwp);
            let c = only(&f, &cfg(&f.state)).expect("checked");
            assert_eq!(c.template_sha256, f.tsha);
            assert_eq!(
                (c.resource_id, c.program_id),
                (Uuid::from_u128(1), Uuid::from_u128(2))
            );
            assert_eq!(c.pdf.is_some(), hwp);
        }
    }

    #[test]
    fn a_finding_a_wrong_nonce_or_an_edited_file_refuses() {
        let f = fixture(
            json!({"nonce":"n0","A":{"verdict":"fail","findings":[{"kind":"missing"}]}}),
            "claude-reviewer",
            false,
        );
        assert!(why(&f).contains("zero findings"));
        let f = fixture(
            json!({"nonce":"old","A":{"verdict":"pass","findings":[]}}),
            "claude-reviewer",
            false,
        );
        assert!(why(&f).contains("nonce"));
        let f = fixture(pass(), "claude-reviewer", false);
        fs::write(f.run.join("out/s.template.json"), b"{\"version\":1}").unwrap();
        assert!(why(&f).contains("changed after review"));
        let f = fixture(pass(), "claude-reviewer", false);
        fs::write(
            f.state.join("verdict/batch.verdict.json"),
            pass().to_string() + " ",
        )
        .unwrap();
        assert!(why(&f).contains("verdict file changed"));
    }

    #[test]
    fn a_verdict_planted_in_the_run_dir_does_not_count() {
        // A parsing step can write the run dir; only the state dir's verdict decides.
        let f = fixture(pass(), "claude-reviewer", false);
        fs::remove_file(f.state.join("verdict/batch.verdict.json")).unwrap();
        fs::write(f.run.join("review/batch.verdict.json"), pass().to_string()).unwrap();
        assert!(why(&f).contains("missing"));
    }

    #[test]
    fn resource_and_pdf_come_from_the_state_dir_not_the_run_dir() {
        // sources.json (run dir) pointing at another resource is refused.
        let f = fixture(pass(), "claude-reviewer", false);
        fs::write(
            f.run.join("sources.json"),
            json!([{"slug":"s","resource_id":Uuid::from_u128(9)}]).to_string(),
        )
        .unwrap();
        assert!(why(&f).contains("not a candidate row"));
        // A converted PDF (and template) swapped after conversion is refused.
        let f = fixture(pass(), "claude-reviewer", true);
        fs::write(
            f.state.join("derived.sha256"),
            format!("s.pdf {}\n", "b".repeat(64)),
        )
        .unwrap();
        assert!(why(&f).contains("converter's output"));
    }

    #[test]
    fn the_judge_decides_only_in_judge_mode() {
        let f = fixture(pass(), "judge", false);
        assert!(why(&f).contains("not the deciding reviewer"));
        let mut c = cfg(&f.state);
        c.allow_judge = true;
        assert!(only(&f, &c).is_ok());
    }

    #[test]
    fn symlinks_and_escapes_are_not_read() {
        let f = fixture(pass(), "claude-reviewer", false);
        let t = f.run.join("out/s.template.json");
        let real = f.run.join("elsewhere.json");
        fs::rename(&t, &real).unwrap();
        std::os::unix::fs::symlink(&real, &t).unwrap();
        assert!(why(&f).contains("symlink"));
        assert!(read_inside(&f.run, Path::new("../etc/passwd"), 10).is_err());
    }

    #[test]
    fn an_empty_manifest_imports_nothing_without_a_key() {
        let f = fixture(pass(), "claude-reviewer", false);
        fs::write(f.state.join("out/import-manifest.json"), b"{\"forms\":[]}").unwrap();
        fs::remove_file(f.state.join("keys/batch.key.json")).unwrap();
        assert!(check(&f.run, &f.state, &cfg(&f.state)).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_kill_dir_counts_as_killed_and_an_unreadable_converter_record_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe_is_root() {
            return; // root reads anything; the Linux check is tests/test_runner.py
        }
        let d = tempdir::Dir::new();
        let dir = d.path().join("ctl");
        fs::create_dir(&dir).unwrap();
        let c = Config {
            actor: Uuid::from_u128(7),
            kill_file: dir.join("stop"),
            allow_judge: false,
        };
        assert!(!c.killed());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(c.killed(), "EACCES must not read as 'not killed'");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let f = fixture(pass(), "claude-reviewer", true);
        fs::set_permissions(
            f.state.join("derived.sha256"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let err = check(&f.run, &f.state, &cfg(&f.state)).unwrap_err();
        assert!(format!("{err:#}").contains("derived.sha256"), "{err:#}");
    }

    fn unsafe_is_root() -> bool {
        std::env::var("USER").map(|u| u == "root").unwrap_or(false)
    }

    #[test]
    fn kill_file_stops_imports() {
        let d = tempdir::Dir::new();
        let c = cfg(d.path());
        assert!(!c.killed());
        fs::write(&c.kill_file, b"").unwrap();
        assert!(c.killed());
    }

    #[test]
    fn audit_rows_name_the_day_and_the_run() {
        // Run names repeat every day (the timer starts at the same minute); the post-check
        // packet is found under $ROOT/.state/<day>/<run>.
        assert_eq!(
            run_label(Path::new("/data/20261026/run-163000")),
            "20261026/run-163000"
        );
        assert_eq!(run_label(Path::new("/data/run-163000")), "run-163000");
        assert_eq!(run_label(Path::new("/data/x1/run-163000")), "run-163000");
    }

    #[test]
    fn a_post_check_answers_ok_or_rollback_only() {
        assert_eq!("ok".parse::<PostCheck>().unwrap(), PostCheck::Ok);
        assert_eq!(
            "rollback".parse::<PostCheck>().unwrap(),
            PostCheck::Rollback
        );
        assert!("pass".parse::<PostCheck>().is_err());
        assert!("".parse::<PostCheck>().is_err());
    }

    #[test]
    fn import_is_off_by_default() {
        std::env::remove_var("FORM_AUTO_IMPORT_ENABLED");
        assert!(Config::from_env().unwrap().is_none());
    }

    /// Post-check bookkeeping on a throwaway database:
    /// `TEST_DATABASE_URL=postgres://… cargo test -p api --lib form_auto_import -- --ignored`
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn post_checks_answer_each_import_once_and_stop_flagging_after_enough_clean_ones() {
        let pool = test_pool().await;
        let actor = Uuid::new_v4();
        let program = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,official_url,raw_payload)
                     VALUES($1,'policy','manual',$2,'post-check test','active',true,'https://example.org/p','{}'::jsonb)")
            .bind(program)
            .bind(format!("pc-{}", program.simple()))
            .execute(&pool)
            .await
            .unwrap();
        let resource: Uuid = sqlx::query_scalar(
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint)
             VALUES($1,'template','https://example.org/f.pdf','https://example.org/n','서식','fp') RETURNING id",
        )
        .bind(program)
        .fetch_one(&pool)
        .await
        .unwrap();
        let sha = "c".repeat(64);
        let import = |pending: bool| {
            let (pool, sha) = (pool.clone(), sha.clone());
            async move {
                let row = Row {
                    action: "publishing",
                    resource: Some(resource),
                    program: Some(program),
                    slug: "s",
                    reviewer: "judge",
                    verdict_sha256: Some(&sha),
                    template_sha256: Some(&sha),
                    official_sha256: Some(&sha),
                    run: "20261026/run-163000",
                    post_check_pending: pending,
                    ..Default::default()
                };
                audit(&pool, actor, row).await.unwrap()
            }
        };
        let base = clean_post_checks(&pool).await.unwrap();
        let first = import(true).await;
        let (rows, _) = pending(&pool).await.unwrap();
        assert!(rows
            .iter()
            .any(|r| r.id == first && r.run == "20261026/run-163000"));
        post_check(
            &pool,
            actor,
            first,
            PostCheck::Ok,
            "template matches the packet",
        )
        .await
        .unwrap();
        assert!(!pending(&pool)
            .await
            .unwrap()
            .0
            .iter()
            .any(|r| r.id == first));
        assert_eq!(clean_post_checks(&pool).await.unwrap(), base + 1);
        // One answer per import.
        assert!(
            post_check(&pool, actor, first, PostCheck::Ok, "answered a second time")
                .await
                .is_err()
        );
        // An import that was not flagged has nothing to answer.
        let unflagged = import(false).await;
        assert!(
            post_check(&pool, actor, unflagged, PostCheck::Ok, "never was pending")
                .await
                .is_err()
        );
        // Rollback when the template is already gone: unpublish is a no-op, the state check
        // passes, one rollback row, the answer is recorded; a second rollback adds no row.
        let bad = import(true).await;
        post_check(
            &pool,
            actor,
            bad,
            PostCheck::Rollback,
            "box 3 is on the wrong line",
        )
        .await
        .unwrap();
        rollback(&pool, actor, resource, "a retried rollback changes nothing")
            .await
            .unwrap();
        let rollbacks: i64 = sqlx::query_scalar("SELECT count(*) FROM form_auto_imports WHERE action='rollback' AND resource_id=$1 AND id>$2")
            .bind(resource)
            .bind(bad)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rollbacks, 1);
        assert!(!pending(&pool).await.unwrap().0.iter().any(|r| r.id == bad));
        assert_eq!(
            clean_post_checks(&pool).await.unwrap(),
            base + 1,
            "a rollback is not a clean post-check"
        );
        // The table stays append-only, and a post_check row must name its import.
        assert!(
            sqlx::query("UPDATE form_auto_imports SET post_check_pending=false WHERE id=$1")
                .bind(first)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query("INSERT INTO form_auto_imports(action,slug,reviewer,run,actor,post_check) VALUES('post_check','s','claude-post-check','r',$1,'ok')")
            .bind(actor)
            .execute(&pool)
            .await
            .is_err());
        assert!(sqlx::query("INSERT INTO form_auto_imports(action,slug,reviewer,run,actor,post_check_pending) VALUES('import','s','-','r',$1,true)")
            .bind(actor)
            .execute(&pool)
            .await
            .is_err());
        // Enough clean answers: the counter reaches POST_CHECKS.
        while clean_post_checks(&pool).await.unwrap() < POST_CHECKS {
            let id = import(true).await;
            post_check(
                &pool,
                actor,
                id,
                PostCheck::Ok,
                "template matches the packet",
            )
            .await
            .unwrap();
        }
        assert!(clean_post_checks(&pool).await.unwrap() >= POST_CHECKS);
    }

    /// The migrations run as the database's admin; the tests then connect as `wello`, a
    /// non-superuser that owns the tables (as the production role does), so FORCE ROW LEVEL
    /// SECURITY, the is_service_role() policies and the grants are what the queries meet.
    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("../../infra/migrations")
            .run(&admin)
            .await
            .unwrap();
        sqlx::query(
            "DO $$ DECLARE t record; BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='wello') THEN
                 CREATE ROLE wello LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD 'wello-test';
               END IF;
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='fa_reader') THEN
                 CREATE ROLE fa_reader NOLOGIN NOSUPERUSER NOBYPASSRLS;
               END IF;
               GRANT USAGE, CREATE ON SCHEMA public TO wello;
               FOR t IN SELECT c.relname, c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
                        WHERE n.nspname='public' AND c.relkind IN ('r','v','m','p') LOOP
                 EXECUTE format('ALTER %s public.%I OWNER TO wello', CASE WHEN t.relkind='v' THEN 'VIEW' WHEN t.relkind='m' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END, t.relname);
               END LOOP;
               -- The production role owns the functions too (some revoke EXECUTE from PUBLIC).
               FOR t IN SELECT p.oid::regprocedure AS sig FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
                        WHERE n.nspname='public'
                          AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid=p.oid AND d.deptype='e') LOOP
                 EXECUTE format('ALTER ROUTINE %s OWNER TO wello', t.sig);
               END LOOP;
               GRANT fa_reader TO wello;
             END $$",
        )
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;
        let (scheme, rest) = url.split_once("://").unwrap();
        let host = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&format!("{scheme}://wello:wello-test@{host}"))
            .await
            .unwrap();
        let (superuser, bypass): (bool, bool) = sqlx::query_as(
            "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname=current_user",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !superuser && !bypass,
            "the DB tests must not run as a superuser"
        );
        pool
    }

    /// RLS is live for the test role: another non-service role sees no audit rows.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn the_audit_table_is_closed_to_roles_other_than_the_service() {
        let pool = test_pool().await;
        let sha = "a".repeat(64);
        audit(
            &pool,
            Uuid::new_v4(),
            Row {
                action: "rollback",
                resource: Some(Uuid::new_v4()),
                slug: "-",
                reviewer: "-",
                official_sha256: Some(&sha),
                run: "-",
                detail: "rls probe",
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("GRANT SELECT ON form_auto_imports TO fa_reader")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SET LOCAL ROLE fa_reader")
            .execute(&mut *tx)
            .await
            .unwrap();
        // Either the policy hides every row, or the role may not even evaluate it.
        match sqlx::query_scalar::<_, i64>("SELECT count(*) FROM form_auto_imports")
            .fetch_one(&mut *tx)
            .await
        {
            Ok(seen) => assert_eq!(
                seen, 0,
                "is_service_role() policy hides every row from another role"
            ),
            Err(e) => assert!(format!("{e}").contains("permission denied"), "{e}"),
        }
        let _ = tx.rollback().await;
    }

    /// A programme with one template resource; returns (programme, resource).
    async fn programme_with_resource(pool: &PgPool) -> (Uuid, Uuid) {
        let program = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,official_url,raw_payload)
                     VALUES($1,'policy','manual',$2,'audit test','active',true,'https://example.org/p','{}'::jsonb)")
            .bind(program)
            .bind(format!("fa-{}", program.simple()))
            .execute(pool)
            .await
            .unwrap();
        let resource: Uuid = sqlx::query_scalar(
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,program_fingerprint)
             VALUES($1,'template','https://example.org/f.pdf','https://example.org/n','서식','fp') RETURNING id",
        )
        .bind(program)
        .fetch_one(pool)
        .await
        .unwrap();
        (program, resource)
    }

    /// The daily pipeline deletes programmes (daily.rs, pipeline.rs: DELETE FROM programs WHERE
    /// id = ANY($1)); resources cascade. The audit must neither block that nor lose its rows.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn deleting_a_programme_keeps_working_and_keeps_its_audit_rows() {
        let pool = test_pool().await;
        let (program, resource) = programme_with_resource(&pool).await;
        let sha = "d".repeat(64);
        let row = Row {
            action: "rollback",
            resource: Some(resource),
            program: Some(program),
            slug: "-",
            reviewer: "-",
            official_sha256: Some(&sha),
            run: "-",
            detail: "regression: programme delete after audit rows",
            ..Default::default()
        };
        let id = audit(&pool, Uuid::new_v4(), row).await.unwrap();
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![program])
            .execute(&pool)
            .await
            .expect("programme delete");
        let kept: (Option<Uuid>, Option<Uuid>, Option<String>) = sqlx::query_as(
            "SELECT resource_id, program_id, official_sha256 FROM form_auto_imports WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            kept,
            (Some(resource), Some(program), Some(sha)),
            "the audit row describes what was deleted, by value"
        );
    }

    /// A template resource the collector checked just now, for these PDF bytes.
    async fn fresh_candidate(pool: &PgPool) -> (Uuid, Uuid) {
        let (program, resource) = programme_with_resource(pool).await;
        let p: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(program)
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE program_application_resources SET content_hash=$2, source_hash=$2, file_format='pdf', byte_size=$3,
                     checked_at=now(), check_status='ok', check_error=NULL, program_fingerprint=$4 WHERE id=$1")
            .bind(resource)
            .bind(digest(PDF))
            .bind(PDF.len() as i64)
            .bind(fingerprint(&p))
            .execute(pool)
            .await
            .unwrap();
        (program, resource)
    }

    async fn state_of(pool: &PgPool, resource: Uuid) -> (bool, String, Option<Uuid>) {
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM application_form_templates WHERE resource_id=$1), r.status, r.reviewed_by
                        FROM program_application_resources r WHERE r.id=$1")
            .bind(resource)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The whole state machine on a real database: dry run, import (audit row first, flagged),
    /// the backlog gate, a post-check rollback that unpublishes and un-verifies, a rollback retry
    /// after a recorded-but-not-unpublished state, and a programme delete afterwards.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn import_post_check_and_rollback_on_a_real_database() {
        let pool = test_pool().await;
        let actor: Uuid = sqlx::query_scalar(
            "INSERT INTO users(id,email) VALUES(gen_random_uuid(), $1) RETURNING id",
        )
        .bind(format!("fa-{}@example.org", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let c = Config {
            actor,
            kill_file: std::env::temp_dir().join("form-auto-no-kill"),
            allow_judge: false,
        };
        let (program, resource) = fresh_candidate(&pool).await;
        let f = fixture_for(pass(), "claude-reviewer", false, resource, program);
        // Dry run: would import, and nothing stays.
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM form_auto_imports")
            .fetch_one(&pool)
            .await
            .unwrap();
        let dry = dry_run(&pool, &c, &f.run, &f.state).await.unwrap();
        assert!(matches!(&dry[0].1, Outcome::WouldImport(_)), "{dry:?}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM form_auto_imports")
                .fetch_one(&pool)
                .await
                .unwrap(),
            before
        );
        assert!(!state_of(&pool, resource).await.0);
        // Import: the publishing row (flagged) comes first, the outcome row points at it.
        let out = apply(&pool, &c, &f.run, &f.state).await.unwrap();
        assert!(
            matches!(
                out[0].1,
                Outcome::Imported {
                    post_check_pending: true,
                    ..
                }
            ),
            "{out:?}"
        );
        let (published, status, by) = state_of(&pool, resource).await;
        assert!(published && status == "verified" && by == Some(actor));
        let (rows, _) = pending(&pool).await.unwrap();
        let attempt = rows
            .iter()
            .find(|r| r.resource_id == Some(resource))
            .expect("pending")
            .id;
        let outcome_attempt: Option<i64> = sqlx::query_scalar(
            "SELECT attempt FROM form_auto_imports WHERE action='import' AND resource_id=$1",
        )
        .bind(resource)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(outcome_attempt, Some(attempt));
        // Backlog: with MAX_PENDING attempts waiting, the next import is refused.
        let sha = "e".repeat(64);
        for _ in 1..MAX_PENDING {
            let r = Row {
                action: "publishing",
                resource: Some(Uuid::new_v4()),
                slug: "x",
                reviewer: "judge",
                verdict_sha256: Some(&sha),
                template_sha256: Some(&sha),
                run: "r",
                post_check_pending: true,
                ..Default::default()
            };
            audit(&pool, actor, r).await.unwrap();
        }
        let (p2, r2) = fresh_candidate(&pool).await;
        let f2 = fixture_for(pass(), "claude-reviewer", false, r2, p2);
        let out = apply(&pool, &c, &f2.run, &f2.state).await.unwrap();
        assert!(
            matches!(&out[0].1, Outcome::Refused(w) if w.contains("post-check backlog")),
            "{out:?}"
        );
        assert!(
            !state_of(&pool, r2).await.0,
            "nothing published past the backlog"
        );
        // Post-check rollback: unpublished, un-verified, recorded, answered.
        post_check(
            &pool,
            actor,
            attempt,
            PostCheck::Rollback,
            "the form is another programme's",
        )
        .await
        .unwrap();
        let (published, status, _) = state_of(&pool, resource).await;
        assert!(!published && status == "rejected", "status {status}");
        let detail: String = sqlx::query_scalar(
            "SELECT detail FROM form_auto_imports WHERE action='rollback' AND resource_id=$1",
        )
        .bind(resource)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(detail.contains("rejected"), "{detail}");
        // The old failure: a rollback row exists but the template is live again (an unpublish
        // that failed after the row). A retry must look at the state, not at the row.
        let (p3, r3) = fresh_candidate(&pool).await;
        let f3 = fixture_for(pass(), "claude-reviewer", false, r3, p3);
        // Publish r3 directly as the importer would (the backlog gate sits in apply), then plant
        // a rollback row first.
        let checked = check(&f3.run, &f3.state, &c)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .unwrap();
        publish(&pool, &c, &checked).await.unwrap();
        audit(
            &pool,
            actor,
            Row {
                action: "rollback",
                resource: Some(r3),
                program: Some(p3),
                slug: "-",
                reviewer: "-",
                official_sha256: Some(&checked.official_sha256),
                run: "-",
                detail: "planted",
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(state_of(&pool, r3).await.0, "live with a rollback row");
        rollback(&pool, actor, r3, "retry after a failed unpublish")
            .await
            .unwrap();
        let (published, status, _) = state_of(&pool, r3).await;
        assert!(!published && status == "rejected");
        // Deleting the programmes keeps working and keeps the audit.
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![program, p2, p3])
            .execute(&pool)
            .await
            .unwrap();
        let kept: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM form_auto_imports WHERE resource_id = ANY($1)",
        )
        .bind(vec![resource, r2, r3])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(kept >= 5);
    }

    /// The dry run and apply share one predicate: a financial-product programme, or a form on
    /// a host the programme does not vouch for, is refused by both and is never verified.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn refused_forms_are_never_verified_and_the_dry_run_says_so_first() {
        let pool = test_pool().await;
        let actor: Uuid = sqlx::query_scalar(
            "INSERT INTO users(id,email) VALUES(gen_random_uuid(), $1) RETURNING id",
        )
        .bind(format!("fa-{}@example.org", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let c = Config {
            actor,
            kill_file: std::env::temp_dir().join("form-auto-no-kill"),
            allow_judge: false,
        };
        for case in ["financial", "foreign_host"] {
            let (program, resource) = fresh_candidate(&pool).await;
            if case == "financial" {
                sqlx::query("UPDATE programs SET program_type='financial_product' WHERE id=$1")
                    .bind(program)
                    .execute(&pool)
                    .await
                    .unwrap();
            } else {
                sqlx::query("UPDATE program_application_resources SET url='https://elsewhere.example.net/f.pdf' WHERE id=$1")
                    .bind(resource).execute(&pool).await.unwrap();
            }
            // Keep the collector's probe fresh for the changed programme fingerprint.
            let p: Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
                .bind(program)
                .fetch_one(&pool)
                .await
                .unwrap();
            sqlx::query(
                "UPDATE program_application_resources SET program_fingerprint=$2 WHERE id=$1",
            )
            .bind(resource)
            .bind(fingerprint(&p))
            .execute(&pool)
            .await
            .unwrap();
            let f = fixture_for(pass(), "claude-reviewer", false, resource, program);
            let dry = dry_run(&pool, &c, &f.run, &f.state).await.unwrap();
            assert!(
                matches!(&dry[0].1, Outcome::Refused(_)),
                "{case}: dry run {dry:?}"
            );
            let out = apply(&pool, &c, &f.run, &f.state).await.unwrap();
            assert!(
                matches!(&out[0].1, Outcome::Refused(_)),
                "{case}: apply {out:?}"
            );
            let (published, status, _) = state_of(&pool, resource).await;
            assert!(
                !published && status == "candidate",
                "{case}: never verified, nothing written ({status})"
            );
        }
    }

    /// `form-auto.sh rollback` (a manual rollback) answers the pending post-checks of the
    /// resource, so the attempt stops holding a backlog slot.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn a_manual_rollback_answers_the_pending_post_check() {
        let pool = test_pool().await;
        let (program, resource) = programme_with_resource(&pool).await;
        let actor = Uuid::new_v4();
        let sha = "f".repeat(64);
        let attempt = audit(
            &pool,
            actor,
            Row {
                action: "publishing",
                resource: Some(resource),
                program: Some(program),
                slug: "s",
                reviewer: "judge",
                verdict_sha256: Some(&sha),
                template_sha256: Some(&sha),
                run: "r",
                post_check_pending: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(pending(&pool)
            .await
            .unwrap()
            .0
            .iter()
            .any(|p| p.id == attempt));
        rollback(&pool, actor, resource, "operator: wrong programme's form")
            .await
            .unwrap();
        assert!(
            !pending(&pool)
                .await
                .unwrap()
                .0
                .iter()
                .any(|p| p.id == attempt),
            "answered by the rollback"
        );
        let answer: (String, String) = sqlx::query_as("SELECT post_check, reviewer FROM form_auto_imports WHERE action='post_check' AND checks_import=$1")
            .bind(attempt).fetch_one(&pool).await.unwrap();
        assert_eq!(answer, ("rollback".to_string(), "-".to_string()));
    }

    /// A verification refused at the commit point (a race: Conflict) takes back the template the
    /// attempt wrote: nothing stays behind that a person's later verification would publish.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn a_refusal_at_verify_removes_the_template_just_written() {
        let pool = test_pool().await;
        let actor: Uuid = sqlx::query_scalar(
            "INSERT INTO users(id,email) VALUES(gen_random_uuid(), $1) RETURNING id",
        )
        .bind(format!("fa-{}@example.org", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let c = Config {
            actor,
            kill_file: std::env::temp_dir().join("form-auto-no-kill"),
            allow_judge: false,
        };
        let (program, resource) = fresh_candidate(&pool).await;
        let f = fixture_for(pass(), "claude-reviewer", false, resource, program);
        FORCE_VERIFY_CONFLICT.store(true, std::sync::atomic::Ordering::SeqCst);
        let out = apply(&pool, &c, &f.run, &f.state).await;
        FORCE_VERIFY_CONFLICT.store(false, std::sync::atomic::Ordering::SeqCst);
        let out = out.unwrap();
        assert!(
            matches!(&out[0].1, Outcome::Refused(w) if w.contains("verification refused")),
            "{out:?}"
        );
        let (published, status, _) = state_of(&pool, resource).await;
        assert!(
            !published && status == "candidate",
            "no template left behind ({status})"
        );
    }

    /// The removal after a refusal at verify takes only the row this attempt wrote: a template a
    /// person saved in the same window (another revision, another author) stays.
    #[tokio::test]
    #[ignore = "needs TEST_DATABASE_URL (a throwaway database)"]
    async fn a_refusal_at_verify_never_removes_a_persons_template() {
        let pool = test_pool().await;
        let actor: Uuid = sqlx::query_scalar(
            "INSERT INTO users(id,email) VALUES(gen_random_uuid(), $1) RETURNING id",
        )
        .bind(format!("fa-{}@example.org", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let person: Uuid = sqlx::query_scalar(
            "INSERT INTO users(id,email) VALUES(gen_random_uuid(), $1) RETURNING id",
        )
        .bind(format!("fa-{}@example.org", Uuid::new_v4().simple()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let c = Config {
            actor,
            kill_file: std::env::temp_dir().join("form-auto-no-kill"),
            allow_judge: false,
        };
        let (program, resource) = fresh_candidate(&pool).await;
        let f = fixture_for(pass(), "claude-reviewer", false, resource, program);
        // The person saves their own template right after the importer's write (the race),
        // simulated by a trigger that rewrites the importer's row as the person's.
        sqlx::query(&format!(
            "CREATE OR REPLACE FUNCTION fa_race() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
               NEW.updated_by := '{person}'; NEW.revision := NEW.revision + 1; RETURN NEW; END $$"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE TRIGGER fa_race BEFORE INSERT ON application_form_templates FOR EACH ROW EXECUTE FUNCTION fa_race()")
            .execute(&pool).await.unwrap();
        FORCE_VERIFY_CONFLICT.store(true, std::sync::atomic::Ordering::SeqCst);
        let out = apply(&pool, &c, &f.run, &f.state).await;
        FORCE_VERIFY_CONFLICT.store(false, std::sync::atomic::Ordering::SeqCst);
        sqlx::query("DROP TRIGGER fa_race ON application_form_templates")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(&out.unwrap()[0].1, Outcome::Refused(_)));
        let kept: Option<Uuid> = sqlx::query_scalar(
            "SELECT updated_by FROM application_form_templates WHERE resource_id=$1",
        )
        .bind(resource)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(kept, Some(person), "the person's template stays");
    }

    /// Minimal temp dir (no extra dependency).
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "form-auto-import-{}",
                    uuid::Uuid::new_v4().simple()
                ));
                std::fs::create_dir(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
