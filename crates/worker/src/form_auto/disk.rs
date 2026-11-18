//! Free-space gate: a run starts only with at least `min_free_gb` free on the
//! filesystem that holds the run directories (the host script checks the same
//! with df before it starts any container).
use anyhow::{anyhow, Context, Result};
use std::path::Path;

/// Free bytes for unprivileged users on the filesystem holding `path` (POSIX `df -Pk`).
pub fn free_bytes(path: &Path) -> Result<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .context("df could not run")?;
    anyhow::ensure!(out.status.success(), "df failed for {}", path.display());
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// The "Available" column (KiB) of the second line of `df -Pk` output, in bytes.
pub fn parse_df(text: &str) -> Result<u64> {
    let line = text
        .lines()
        .nth(1)
        .ok_or_else(|| anyhow!("df printed no filesystem line"))?;
    let avail = line
        .split_whitespace()
        .nth(3)
        .ok_or_else(|| anyhow!("df line has no Available column"))?;
    Ok(avail
        .parse::<u64>()
        .context("df Available is not a number")?
        * 1024)
}

/// `Ok(free_gb)` when the run may start, `Err` with the reason otherwise.
pub fn gate(free: u64, min_free_gb: u64) -> std::result::Result<u64, String> {
    let gb = free / (1024 * 1024 * 1024);
    if gb < min_free_gb.max(super::config::MIN_FREE_GB) {
        Err(format!(
            "{gb} GB free < {min_free_gb} GB: run skipped (server floor {} GB)",
            super::config::DISK_FLOOR_GB
        ))
    } else {
        Ok(gb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_posix_df_and_skips_below_the_margin() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda3 66000000 40000000 23068672 64% /\n";
        let free = parse_df(df).unwrap();
        assert_eq!(free, 23068672 * 1024);
        assert_eq!(gate(free, 22), Ok(22));
        assert!(gate(21 * 1024 * 1024 * 1024, 22).is_err());
        // A caller can never ask for less than the fixed 22 GB margin.
        assert!(gate(21 * 1024 * 1024 * 1024, 5).is_err());
        assert!(parse_df("garbage").is_err());
    }
}
