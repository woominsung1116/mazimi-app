//! The worker part of the server-side automatic form-template job, called by
//! `scripts/forms/server/form-auto.sh` between its sandboxed steps:
//!
//!   form-auto candidates <out.json>     read-only candidate query (when FORM_AUTO_ENABLED=true)
//!   form-auto disk <dir>                exit 3 when fewer than FORM_AUTO_MIN_FREE_GB (>= 22) are free
//!   form-auto sweep <dir>               keep only regular, size-capped, valid step outputs
//!   form-auto record <run dir> <done|skipped|failed> [reason]   run event + job:form_auto incident
use anyhow::{bail, Context, Result};
use mazimi_worker::form_auto::{candidates, disk, outputs, record, Config};
use mazimi_worker::runtime;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let env = runtime::environment()?;
    runtime::init_tracing(&env);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(cfg) = Config::from_env()? else {
        println!("form auto is off (FORM_AUTO_ENABLED != true)");
        return Ok(());
    };
    match a.as_slice() {
        ["candidates", out] => {
            let pool = runtime::connect_job_pool().await?;
            let rows = candidates::fetch(&pool).await?;
            pool.close().await;
            let path = PathBuf::from(out);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).context("output must be new")?;
            std::io::Write::write_all(&mut f, serde_json::to_string(&rows)?.as_bytes())?;
            println!("{} candidate rows", rows.as_array().map_or(0, Vec::len));
        }
        ["disk", dir] => match disk::gate(disk::free_bytes(&PathBuf::from(dir))?, cfg.min_free_gb) {
            Ok(gb) => println!("{gb} GB free"),
            Err(why) => {
                println!("{why}");
                std::process::exit(3);
            }
        },
        ["sweep", dir] => {
            let s = outputs::sweep(&PathBuf::from(dir))?;
            println!("kept {} removed {}", s.kept, s.removed.len());
        }
        ["record", run, status, rest @ ..] => {
            let run = PathBuf::from(run);
            // <day>/<run-HHMMSS>: the day keeps run fingerprints unique across days.
            let part = |p: Option<&std::path::Path>| p.and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("-").to_string();
            let name = format!("{}/{}", part(run.parent()), part(Some(run.as_path())));
            let read = |p: PathBuf| -> Option<serde_json::Value> {
                let meta = std::fs::symlink_metadata(&p).ok()?;
                (meta.is_file() && meta.len() <= 8 * 1024 * 1024).then(|| std::fs::read(&p).ok()).flatten()
                    .and_then(|b| serde_json::from_slice(&b).ok())
            };
            let verdicts = read(run.join("verdicts.json")).unwrap_or_default();
            let import = read(run.join("import.json"));
            let converters = read(run.join("converters.json"));
            let pool = runtime::connect_job_pool().await?;
            let outcome = record::Outcome::parse(status, &rest.join(" "));
            record::record(&pool, &name, &outcome, record::summary(&verdicts, import.as_ref(), converters.as_ref())).await?;
            pool.close().await;
            println!("recorded {status} for {name}");
        }
        _ => bail!("usage: form-auto candidates <out> | disk <dir> | sweep <dir> | record <run> <done|skipped|failed> [reason]"),
    }
    Ok(())
}
