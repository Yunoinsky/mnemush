// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Self-eval NDJSON log maintenance.
//!
//! Each agent plugin (Pi, OpenCode) writes one NDJSON file per session
//! to `~/.mnemush/eval/<session>.ndjson`. Over time these accumulate:
//!   - heavy users generate many entries per session
//!   - many users accumulate many session files
//!   - old sessions stop being useful for trend analysis
//!
//! [`prune_apply`] enforces three caps from [`EvalConfig`]:
//!   1. `max_age_days` — drop files older than this (TTL).
//!   2. `max_entries_per_file` — keep only the most recent N lines per file.
//!   3. `max_session_files` — keep at most N session files total.
//!
//! Apply order is TTL → per-file trim → file-count cap. Each step uses
//! only the data the previous step left behind, so the bounds compose.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::EvalConfig;
use crate::error::{MnemushError, Result};

/// Path to the per-session eval NDJSON directory.
pub fn eval_dir() -> PathBuf {
    crate::default_data_dir().join("eval")
}

/// Summary of a prune pass. Returned by [`prune_dry_run`] and
/// [`prune_apply`] so the caller (CLI, agent plugin) can surface it.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PruneReport {
    /// Files removed because their mtime was older than `max_age_days`.
    pub files_removed_age: usize,
    /// Files removed because keeping the newest `max_session_files` would exceed the cap.
    pub files_removed_count: usize,
    /// Total lines dropped across all surviving files (per-file line cap).
    pub lines_dropped_count: usize,
    /// Number of surviving files.
    pub files_kept: usize,
    /// Total surviving lines across all files.
    pub lines_kept: usize,
    /// Total bytes saved (estimated: lines_dropped × avg_bytes_per_line).
    /// Best-effort — actual savings depend on filesystem block size.
    pub bytes_recovered_estimated: u64,
}

/// Inspect the eval directory and report what a prune would do, without
/// writing anything. Use this from the CLI's dry-run mode.
pub fn prune_dry_run(cfg: &EvalConfig) -> Result<PruneReport> {
    let dir = eval_dir();
    if !dir.exists() {
        return Ok(PruneReport::default());
    }
    prune_inner(&dir, cfg, /*apply=*/ false)
}

/// Same as [`prune_dry_run`] but actually deletes/rewrites files. Safe
/// to call from session_end (idempotent — running it twice in a row is
/// a no-op).
pub fn prune_apply(cfg: &EvalConfig) -> Result<PruneReport> {
    let dir = eval_dir();
    if !dir.exists() {
        // Create the dir so subsequent writes don't fail. Cheap.
        fs::create_dir_all(&dir).map_err(|e| {
            MnemushError::Other(format!("create eval dir {}: {}", dir.display(), e))
        })?;
        return Ok(PruneReport::default());
    }
    prune_inner(&dir, cfg, /*apply=*/ true)
}

fn prune_inner(dir: &Path, cfg: &EvalConfig, apply: bool) -> Result<PruneReport> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let age_cutoff_secs = cfg.max_age_days.saturating_mul(86_400);

    // Collect (path, mtime_secs).
    let mut files: Vec<(PathBuf, i64)> = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|e| MnemushError::Other(format!("read_dir {}: {}", dir.display(), e)))?
    {
        let entry = entry.map_err(|e| MnemushError::Other(e.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
            continue;
        }
        let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
            Ok(t) => t
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            // Ponytail bug fix: silently defaulting mtime=0 made every
            // file look ancient (epoch = 1970), so the age cap dropped
            // everything. If we can't read mtime, treat the file as
            // new (now) — better to over-keep than to nuke live data.
            Err(_) => now,
        };
        files.push((path, mtime));
    }

    let mut report = PruneReport::default();

    // Step 1: TTL — drop files older than max_age_days.
    let mut survivors: Vec<(PathBuf, i64)> = Vec::with_capacity(files.len());
    for (path, mtime) in files {
        if cfg.max_age_days > 0 && (now - mtime) > age_cutoff_secs {
            if apply {
                fs::remove_file(&path).map_err(|e| {
                    MnemushError::Other(format!("remove {}: {}", path.display(), e))
                })?;
            }
            report.files_removed_age += 1;
        } else {
            survivors.push((path, mtime));
        }
    }

    // Step 2: per-file line cap. Read each file, keep only the last
    // `max_entries_per_file` non-empty lines, rewrite if any dropped.
    // Track per-file line counts so step 3 can subtract dropped files
    // from the total when the file-count cap fires.
    let mut file_lines: std::collections::HashMap<PathBuf, usize> =
        std::collections::HashMap::new();
    let mut total_lines_kept: usize = 0;
    for (path, _) in &survivors {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let non_empty: Vec<&str> = content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let total = non_empty.len();
        if total > cfg.max_entries_per_file {
            let drop_n = total - cfg.max_entries_per_file;
            report.lines_dropped_count += drop_n;
            // Estimate bytes: total bytes / total lines × drop_n.
            let avg = content.len().checked_div(total).unwrap_or(0);
            report.bytes_recovered_estimated += (drop_n * avg) as u64;
            if apply {
                let kept: Vec<&str> = non_empty.iter().skip(drop_n).copied().collect();
                let mut f = fs::File::create(path).map_err(|e| {
                    MnemushError::Other(format!("rewrite {}: {}", path.display(), e))
                })?;
                for line in kept {
                    writeln!(f, "{}", line).map_err(|e| {
                        MnemushError::Other(format!("write {}: {}", path.display(), e))
                    })?;
                }
            }
            total_lines_kept += cfg.max_entries_per_file;
            file_lines.insert(path.clone(), cfg.max_entries_per_file);
        } else {
            total_lines_kept += total;
            file_lines.insert(path.clone(), total);
        }
    }

    // Step 3: file-count cap. Sort survivors by mtime DESC, keep the
    // first `max_session_files`, remove the rest. Subtract the lines
    // of removed files from `total_lines_kept` so `lines_kept` reflects
    // reality.
    use std::cmp::Reverse;
    survivors.sort_by_key(|(_, mtime)| Reverse(*mtime));
    if survivors.len() > cfg.max_session_files {
        for (path, _) in survivors.iter().skip(cfg.max_session_files) {
            if apply {
                fs::remove_file(path).map_err(|e| {
                    MnemushError::Other(format!("remove {}: {}", path.display(), e))
                })?;
            }
            report.files_removed_count += 1;
            if let Some(n) = file_lines.get(path) {
                total_lines_kept = total_lines_kept.saturating_sub(*n);
            }
        }
        report.files_kept = cfg.max_session_files;
    } else {
        report.files_kept = survivors.len();
    }
    report.lines_kept = total_lines_kept;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_session(dir: &Path, session: &str, lines: usize, age_secs: i64) {
        let path = dir.join(format!("{}.ndjson", session));
        let mut f = fs::File::create(&path).unwrap();
        for i in 0..lines {
            // Use a per-line ts so the parser doesn't choke.
            writeln!(
                f,
                r#"{{"ts":{},"session":"{}","i":{}}}"#,
                age_secs, session, i
            )
            .unwrap();
        }
        // Backdate mtime to simulate age.
        let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(age_secs as u64);
        filetime_set(&path, mtime);
    }

    /// Set the mtime of a file. Uses std::fs::File::set_modified
    /// (stable since Rust 1.75) — no external dependency.
    fn filetime_set(path: &Path, t: SystemTime) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mnemush-eval-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = fresh_dir("dryrun");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        write_session(&dir, "s1", 100, now); // current mtime → keep
        write_session(&dir, "s2", 5_000, now);
        let cfg = EvalConfig {
            max_age_days: 30,
            max_entries_per_file: 1000,
            max_session_files: 10,
        };
        let r = prune_dry_run_with_dir(&dir, &cfg).unwrap();
        assert_eq!(r.files_kept, 2);
        // Files still exist with original line counts.
        let s1_lines = fs::read_to_string(dir.join("s1.ndjson"))
            .unwrap()
            .lines()
            .count();
        let s2_lines = fs::read_to_string(dir.join("s2.ndjson"))
            .unwrap()
            .lines()
            .count();
        assert_eq!(s1_lines, 100);
        assert_eq!(s2_lines, 5_000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn applies_all_three_caps() {
        let dir = fresh_dir("apply");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 5 sessions: 2 fresh, 2 old (age 100d), 1 fresh but too big.
        // 各 fresh 文件使用不同的 mtime,打破 stable-sort 平局——Windows 上
        // mtime 精度为秒且 read_dir 顺序受文件系统影响,同 mtime 下断言
        // 不确定性(可能丢 huge 也可能丢 fresh)。实际逻辑不受影响,仅断言
        // 顺序性需要 mtime 区分。
        write_session(&dir, "fresh1", 10, now - 30);
        write_session(&dir, "fresh2", 10, now - 20);
        write_session(&dir, "old1", 10, now - 100 * 86_400);
        write_session(&dir, "old2", 10, now - 100 * 86_400);
        write_session(&dir, "huge", 5_000, now);
        let cfg = EvalConfig {
            max_age_days: 30,
            max_entries_per_file: 1000,
            max_session_files: 2,
        };
        let r = prune_apply_with_dir(&dir, &cfg).unwrap();
        // 2 old removed by age, 1 by count cap (after age-sort, fresh2 + huge → keep 2, fresh1 dropped as oldest fresh).
        assert_eq!(r.files_removed_age, 2, "old1 + old2 should be aged out");
        assert_eq!(
            r.files_removed_count, 1,
            "after age: 3 survivors, cap=2 → drop 1"
        );
        // huge file trimmed from 5000 → 1000 lines.
        assert_eq!(r.lines_dropped_count, 4_000);
        // After trim + count cap, huge (trimmed to 1000) + fresh2 (10) survive.
        assert_eq!(r.lines_kept, 10 + 1000);
        // fresh1, fresh2 or huge should still exist; old1/old2 gone.
        assert!(!dir.join("old1.ndjson").exists());
        assert!(!dir.join("old2.ndjson").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Variant of [`prune_dry_run`] that takes an explicit dir (used
    /// by tests that want to point at a temp dir, not the real one).
    fn prune_dry_run_with_dir(dir: &Path, cfg: &EvalConfig) -> Result<PruneReport> {
        prune_inner(dir, cfg, /*apply=*/ false)
    }

    fn prune_apply_with_dir(dir: &Path, cfg: &EvalConfig) -> Result<PruneReport> {
        prune_inner(dir, cfg, /*apply=*/ true)
    }
}
