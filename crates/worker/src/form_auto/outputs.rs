//! Step outputs come out of containers that parsed untrusted agency files, so
//! they are read back like untrusted input: only regular files (never a
//! symlink, device or directory planted in an output directory), a size cap
//! per kind, and JSON that parses. Anything else is removed from the run
//! directory before a later step or the importer can read it.
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Size cap by extension (JSON and PDF 8 MiB, PNG 20 MiB, HWP/HWPX 16 MiB, text 1 MiB);
/// any other kind of file is not a step output.
pub fn cap(name: &str) -> Option<u64> {
    let mib = 1024 * 1024;
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "json" => 8 * mib,
        "pdf" => 8 * mib,
        "png" => 20 * mib,
        "hwp" | "hwpx" => 16 * mib,
        "txt" | "tsv" => mib,
        _ => return None,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    pub kept: usize,
    pub removed: Vec<PathBuf>,
}

/// Walks a step's output directory: keeps regular files of known kinds within
/// their cap whose JSON parses; removes everything else (and empty subdirs are left).
pub fn sweep(dir: &Path) -> Result<Sweep> {
    let mut out = Sweep::default();
    walk(dir, 0, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, depth: usize, out: &mut Sweep) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_dir() && !ft.is_symlink() && depth < 3 {
            walk(&path, depth + 1, out)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let ok = ft.is_file()
            && cap(&name).is_some_and(|c| meta.len() > 0 && meta.len() <= c)
            && (!name.ends_with(".json")
                || std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .is_some());
        if ok {
            out.kept += 1;
        } else {
            if ft.is_dir() && !ft.is_symlink() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            out.removed.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("form-auto-out-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&p).unwrap();
        p
    }

    #[test]
    fn only_regular_valid_capped_files_survive() {
        let d = tmp();
        fs::write(d.join("a.ir.json"), b"{\"version\":1}").unwrap();
        fs::write(d.join("bad.json"), b"{not json").unwrap();
        fs::write(d.join("empty.pdf"), b"").unwrap();
        fs::write(d.join("x.sh"), b"#!/bin/sh").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.join("key.json")).unwrap();
        let s = sweep(&d).unwrap();
        assert_eq!(s.kept, 1);
        assert_eq!(s.removed.len(), 4);
        assert!(d.join("a.ir.json").exists() && fs::symlink_metadata(d.join("key.json")).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
