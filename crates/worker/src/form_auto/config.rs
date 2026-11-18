//! Settings of the automatic form-template job, from the environment.
//! Everything is off unless `FORM_AUTO_ENABLED=true`.
use anyhow::{bail, ensure, Context, Result};
use std::path::PathBuf;

/// The server's disk floor. Never lowered: the job keeps a margin above it.
pub const DISK_FLOOR_GB: u64 = 20;
/// Default (and minimum) free space a run needs before it starts.
pub const MIN_FREE_GB: u64 = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// The judge does not run.
    Off,
    /// The judge runs and is logged next to the independent reviewer, which decides.
    Shadow,
    /// The judge decides (verdicts.py admits it only after its recorded gate).
    Judge,
}

impl ReviewMode {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value.trim() {
            "" | "shadow" => Self::Shadow,
            "off" => Self::Off,
            "judge" => Self::Judge,
            other => bail!("FORM_REVIEW_JUDGE must be off, shadow or judge, not {other:?}"),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Judge => "judge",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Parent of the run directories (one new directory per run).
    pub run_root: PathBuf,
    pub min_free_gb: u64,
    pub max_forms: u32,
    pub review: ReviewMode,
    /// Publish what passed (FORM_AUTO_IMPORT_ENABLED); otherwise runs stop at verdicts.
    pub import: bool,
    /// Days a run directory is kept before it is reduced to its JSON verdicts.
    pub keep_days: u32,
}

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn bounded(key: &str, default: u64, lo: u64, hi: u64) -> Result<u64> {
    let v = match var(key) {
        Some(v) => v
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{key} is not a number"))?,
        None => default,
    };
    ensure!(
        (lo..=hi).contains(&v),
        "{key} must be between {lo} and {hi}"
    );
    Ok(v)
}

impl Config {
    /// `Ok(None)` unless `FORM_AUTO_ENABLED=true`. A set but invalid value is an
    /// error, so a typo stops the job instead of silently changing it.
    pub fn from_env() -> Result<Option<Self>> {
        match var("FORM_AUTO_ENABLED").as_deref() {
            None | Some("false") => return Ok(None),
            Some("true") => {}
            Some(other) => bail!("FORM_AUTO_ENABLED must be true or false, not {other:?}"),
        }
        let run_root = PathBuf::from(
            var("FORM_AUTO_RUN_ROOT").unwrap_or_else(|| "/var/lib/mazimi/form-auto".into()),
        );
        ensure!(
            run_root.is_absolute(),
            "FORM_AUTO_RUN_ROOT must be absolute"
        );
        Ok(Some(Self {
            run_root,
            // The 20 GB floor is never lowered: a run needs at least 22 GB free.
            min_free_gb: bounded("FORM_AUTO_MIN_FREE_GB", MIN_FREE_GB, MIN_FREE_GB, 500)?,
            max_forms: bounded("FORM_AUTO_MAX_FORMS", 10, 1, 100)? as u32,
            review: ReviewMode::parse(&var("FORM_REVIEW_JUDGE").unwrap_or_default())?,
            import: match var("FORM_AUTO_IMPORT_ENABLED").as_deref() {
                None | Some("false") => false,
                Some("true") => true,
                Some(other) => {
                    bail!("FORM_AUTO_IMPORT_ENABLED must be true or false, not {other:?}")
                }
            },
            keep_days: bounded("FORM_AUTO_KEEP_DAYS", 7, 1, 30)? as u32,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    fn with(vars: &[(&str, &str)], f: impl FnOnce()) {
        let _g = ENV.lock().unwrap();
        let keys = [
            "FORM_AUTO_ENABLED",
            "FORM_AUTO_RUN_ROOT",
            "FORM_AUTO_MIN_FREE_GB",
            "FORM_AUTO_MAX_FORMS",
            "FORM_REVIEW_JUDGE",
            "FORM_AUTO_IMPORT_ENABLED",
            "FORM_AUTO_KEEP_DAYS",
        ];
        for k in keys {
            std::env::remove_var(k);
        }
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        f();
        for k in keys {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn off_by_default_and_shadow_review_when_on() {
        with(&[], || assert!(Config::from_env().unwrap().is_none()));
        with(&[("FORM_AUTO_ENABLED", "true")], || {
            let c = Config::from_env().unwrap().unwrap();
            assert_eq!(c.review, ReviewMode::Shadow);
            assert!(!c.import);
            assert_eq!(c.min_free_gb, 22);
        });
    }

    #[test]
    fn the_disk_margin_cannot_be_lowered_and_typos_stop() {
        with(
            &[
                ("FORM_AUTO_ENABLED", "true"),
                ("FORM_AUTO_MIN_FREE_GB", "20"),
            ],
            || assert!(Config::from_env().is_err()),
        );
        with(&[("FORM_AUTO_ENABLED", "yes")], || {
            assert!(Config::from_env().is_err())
        });
        with(
            &[("FORM_AUTO_ENABLED", "true"), ("FORM_REVIEW_JUDGE", "auto")],
            || assert!(Config::from_env().is_err()),
        );
        with(
            &[
                ("FORM_AUTO_ENABLED", "true"),
                ("FORM_AUTO_RUN_ROOT", "runs"),
            ],
            || assert!(Config::from_env().is_err()),
        );
    }
}
