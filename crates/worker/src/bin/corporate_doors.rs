//! The corporate doors' numbers (review H7).
//!
//! ```text
//! corporate-doors metrics [--days N]       # the shadow gate metrics (reads DATABASE_URL)
//! corporate-doors fixtures <dir> [--today YYYY-MM-DD]
//!                                          # the readers on saved pages, no request
//! ```
//!
//! `metrics` prints, per lane, the reads and verdicts, the offers accepted
//! (the ground-truth sample frame), the must-have brands accepted, the
//! rejection reasons, the identical re-read share and the source agreement
//! pairs, with the automatic gate verdict. `fixtures` runs the same readers
//! on saved hub list pages (`<dir>/hubs/*.html`, the grantor named by the
//! file's prefix) and first-party pages (`<dir>/pages/*.html`, the brand
//! named by the host) with the database's brand registry, and prints the
//! same per-lane counts. Read-only in both modes: it never writes and never
//! fetches.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use mazimi_worker::corporate_auto::bootstrap::{self, BrandScope, Scope};
use mazimi_worker::corporate_auto::doors::{metrics, registry::Registry, rows};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await?;
    match command.as_str() {
        "metrics" => {
            let mut days = 14;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--days" => days = args.next().context("--days needs a value")?.parse()?,
                    other => bail!("unknown argument {other}"),
                }
            }
            let m = metrics::collect(&pool, days).await?;
            print!("{}", metrics::render(&m));
        }
        "fixtures" => {
            let dir = PathBuf::from(args.next().context("fixtures needs a directory")?);
            let mut today = NaiveDate::from_ymd_opt(2026, 11, 11).expect("date");
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--today" => today = args.next().context("--today needs a value")?.parse()?,
                    other => bail!("unknown argument {other}"),
                }
            }
            let registry = Registry::load(&pool).await?;
            print!("{}", fixtures(&dir, &registry, today)?);
        }
        _ => {
            bail!("usage: corporate-doors metrics [--days N] | fixtures <dir> [--today YYYY-MM-DD]")
        }
    }
    Ok(())
}

/// The registry brand a fixture file name starts with
/// (`sktmembership_…` → SK텔레콤), by the hosts the census saved them from.
fn grantor_of(file: &str) -> Option<&'static str> {
    [
        ("sktmembership", "SK텔레콤"),
        ("kbcard", "KB국민카드"),
        ("payco", "PAYCO"),
        ("gong-cha", "공차"),
    ]
    .into_iter()
    .find(|(prefix, _)| file.starts_with(prefix))
    .map(|(_, brand)| brand)
}

fn fixtures(dir: &std::path::Path, registry: &Registry, today: NaiveDate) -> Result<String> {
    let mut out = String::new();
    let name_of = |id: Uuid| {
        registry
            .get(id)
            .map_or_else(|| id.to_string(), |b| b.name.clone())
    };
    let brand_named = |name: &str| registry.brands().iter().find(|b| b.name == name);
    let mut must_have: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("hubs"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    files.sort();
    for path in files {
        let file = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Some(grantor) = grantor_of(&file).and_then(brand_named) else {
            out.push_str(&format!("{file}: no grantor in the registry\n"));
            continue;
        };
        let html = std::fs::read_to_string(&path)?;
        let read = rows::read_hub(&html, registry, grantor.brand_id, today);
        out.push_str(&format!(
            "hub_rows {file} (grantor {}): {} rows, {} matched, {} tiered, {} accepted\n",
            grantor.name,
            read.rows_seen,
            read.rows_matched,
            read.tiered_rows,
            read.offers.len()
        ));
        for offer in &read.offers {
            let brand = name_of(offer.brand_id);
            out.push_str(&format!(
                "  accepted: {} | {} | {:?}{}{}\n",
                offer.title(&grantor.name),
                brand,
                offer.tiers,
                offer
                    .starts
                    .map_or(String::new(), |s| format!(" | starts {s}")),
                offer.ends.map_or(String::new(), |e| format!(" | ends {e}"))
            ));
            if registry.get(offer.brand_id).is_some_and(|b| b.must_have) {
                must_have.entry("hub_rows").or_default().push(brand);
            }
        }
        out.push_str(&format!("  rejected by reason: {:?}\n", read.rejected));
    }
    let mut pages: Vec<PathBuf> = std::fs::read_dir(dir.join("pages"))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "html"))
                .collect()
        })
        .unwrap_or_default();
    pages.sort();
    for path in pages {
        let file = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Some(brand) = grantor_of(&file).and_then(brand_named) else {
            out.push_str(&format!("{file}: no brand in the registry\n"));
            continue;
        };
        let html = std::fs::read_to_string(&path)?;
        let scope = Scope::Brand(BrandScope::new(
            brand.brand_id,
            &brand.name,
            Uuid::nil(),
            registry,
        ));
        match bootstrap::extract_offer_scoped(&html, today, &scope) {
            Ok(offer) => {
                let title =
                    mazimi_worker::corporate_auto::publish::offer_title(&brand.name, &offer)
                        .unwrap_or_else(|reason| format!("(no title: {reason})"));
                out.push_str(&format!(
                    "page {file} (brand {}): accepted {title}; issuer {}\n",
                    brand.name,
                    offer
                        .issuer
                        .as_ref()
                        .map_or("none".to_string(), |i| format!("{} ({})", i.name, i.line))
                ));
                if brand.must_have {
                    must_have
                        .entry("page")
                        .or_default()
                        .push(brand.name.clone());
                }
            }
            Err(reason) => out.push_str(&format!("page {file} (brand {}): {reason}\n", brand.name)),
        }
    }
    for (lane, mut brands) in must_have {
        brands.sort();
        brands.dedup();
        out.push_str(&format!(
            "must-have brands accepted, lane {lane}: {} [{}]\n",
            brands.len(),
            brands.join(", ")
        ));
    }
    Ok(out)
}
