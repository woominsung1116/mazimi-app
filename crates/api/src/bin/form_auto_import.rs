//! Automatic import of reviewed form templates, and its rollback.
//!
//!   form-auto-import apply <run dir> <state dir>      # publish what verdicts.py made importable
//!   form-auto-import rollback <resource uuid> <reason>  # unpublish one template now
//!   form-auto-import check <run dir> <state dir>      # dry run against the real database: nothing stays
//!   form-auto-import pending                            # imports waiting for their post-check (JSON lines)
//!   form-auto-import postcheck <import id> ok|rollback <reason>
//!
//! Runs as a one-shot in the API image (it reuses the admin services, so every
//! server-side check of a manual import applies). See api::form_auto_import.
use api::form_auto_import::{self, Config, Outcome, PostCheck, MAX_PENDING, POST_CHECKS};
use mazimi_core::{connect_database, DatabaseConfig, PoolRole};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["apply", run, state] => {
            let Some(cfg) = Config::from_env()? else {
                println!("form auto import is off (FORM_AUTO_IMPORT_ENABLED != true)");
                return Ok(());
            };
            if cfg.killed() {
                println!("kill switch {} present: nothing imported", cfg.kill_file.display());
                return Ok(());
            }
            let pool = connect_database(&DatabaseConfig::from_env(PoolRole::OneShot)?).await?;
            let results = form_auto_import::apply(&pool, &cfg, &PathBuf::from(run), &PathBuf::from(state)).await?;
            let (mut refused, mut pending) = (0, 0);
            for (slug, outcome) in &results {
                // Plain slugs and outcome codes only (slugs are [a-z0-9-]); reasons are in form_auto_imports.
                let slug: String = slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
                match outcome {
                    Outcome::Imported { revision, post_check_pending } => {
                        pending += usize::from(*post_check_pending);
                        println!("{slug} imported revision {revision}{}", if *post_check_pending { " (post-check pending)" } else { "" })
                    }
                    Outcome::UpToDate => println!("{slug} up to date"),
                    Outcome::Killed => println!("{slug} not imported: kill switch"),
                    Outcome::WouldImport(_) => {}
                    Outcome::Refused(_) => {
                        refused += 1;
                        println!("{slug} refused (see form_auto_imports)")
                    }
                }
            }
            let backlog = form_auto_import::pending(&pool).await?.0.len();
            pool.close().await;
            println!("{}", serde_json::json!({"forms": results.len(), "refused": refused, "post_check_pending": pending, "post_check_backlog": backlog}));
            Ok(())
        }
        ["check", run, state] => {
            let cfg = Config::for_dry_run()?;
            let pool = connect_database(&DatabaseConfig::from_env(PoolRole::OneShot)?).await?;
            let results = form_auto_import::dry_run(&pool, &cfg, &PathBuf::from(run), &PathBuf::from(state)).await?;
            let backlog = form_auto_import::pending(&pool).await?.0.len();
            pool.close().await;
            let mut would = 0;
            for (slug, outcome) in &results {
                let slug: String = slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
                match outcome {
                    Outcome::WouldImport(what) => {
                        would += 1;
                        println!("{slug} {what}")
                    }
                    Outcome::Refused(why) => println!("{slug} would be refused: {}", why.chars().take(300).collect::<String>()),
                    _ => {}
                }
            }
            println!("{}", serde_json::json!({"dry_run": true, "forms": results.len(), "would_import": would,
                "refused": results.len() - would, "post_check_backlog": backlog, "max_pending": MAX_PENDING}));
            Ok(())
        }
        ["rollback", resource, reason] => {
            let actor = std::env::var("FORM_AUTO_ACTOR_USER_ID")?.trim().parse::<Uuid>()?;
            let pool = connect_database(&DatabaseConfig::from_env(PoolRole::OneShot)?).await?;
            form_auto_import::rollback(&pool, actor, resource.parse()?, reason).await?;
            pool.close().await;
            println!("unpublished {resource}");
            Ok(())
        }
        ["pending"] => {
            let pool = connect_database(&DatabaseConfig::from_env(PoolRole::OneShot)?).await?;
            let (rows, clean) = form_auto_import::pending(&pool).await?;
            pool.close().await;
            for r in &rows {
                println!("{}", serde_json::to_string(r)?);
            }
            println!("{}", serde_json::json!({"pending": rows.len(), "clean_post_checks": clean, "flagging_until": POST_CHECKS}));
            Ok(())
        }
        ["postcheck", id, answer, reason] => {
            let actor = std::env::var("FORM_AUTO_ACTOR_USER_ID")?.trim().parse::<Uuid>()?;
            let answer: PostCheck = answer.parse()?;
            let pool = connect_database(&DatabaseConfig::from_env(PoolRole::OneShot)?).await?;
            form_auto_import::post_check(&pool, actor, id.parse()?, answer, reason).await?;
            pool.close().await;
            println!("post-check of import {id} recorded: {}", if answer == PostCheck::Ok { "ok" } else { "rolled back" });
            Ok(())
        }
        _ => anyhow::bail!(
            "usage: form-auto-import apply <run dir> <state dir> | check <run dir> <state dir> | rollback <resource uuid> <reason> | pending | postcheck <import id> ok|rollback <reason>"
        ),
    }
}
