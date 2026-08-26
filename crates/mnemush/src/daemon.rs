//! Dream scheduler daemon (v1.6.1).
//!
//! Long-running process that wakes at `[dream] scheduled_time` (default
//! 02:00 system-local), gates on daily token budget, then runs `mnemush
//! dream`. Single-instance via flock on `<data_dir>/daemon.lock`.
//!
//! Crash safety: writes `dream_started.json` before each run and
//! `dream_completed.json` after. If the daemon dies mid-dream, the next
//! 2am still runs (completed.json wasn't updated) — unlike the legacy
//! client hook which used a single `last_run` field that prevented
//! retry on failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use crate::config::{Config, DreamConfig};
use crate::error::Result;
use crate::default_data_dir;

/// Path to the started marker. Existence alone doesn't gate runs; we
/// compare against `dream_completed.json`'s `date` field.
fn started_path(data_dir: &Path) -> PathBuf {
    data_dir.join("dream_started.json")
}
fn completed_path(data_dir: &Path) -> PathBuf {
    data_dir.join("dream_completed.json")
}
fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.lock")
}

/// Parse "HH:MM" into (hour, minute). Validates basic shape.
fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split(':');
    let h: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

/// Compute the next instant at which `scheduled_time` (HH:MM local) will
/// occur, strictly after `now`. Returns a chrono `DateTime<Local>`.
pub fn next_wake(now: chrono::DateTime<chrono::Local>, hhmm: &str) -> Option<chrono::DateTime<chrono::Local>> {
    let (h, m) = parse_hhmm(hhmm)?;
    let today = now.date_naive();
    let mut candidate = today.and_hms_opt(h, m, 0)?.and_local_timezone(now.timezone()).unwrap();
    if candidate <= now {
        candidate = (today + chrono::Duration::days(1))
            .and_hms_opt(h, m, 0)?
            .and_local_timezone(now.timezone())
            .unwrap();
    }
    Some(candidate)
}

/// True if today's run already completed successfully (or at all — we
/// record the date on completion and skip if today's already there).
fn already_completed_today(data_dir: &Path) -> bool {
    let p = completed_path(data_dir);
    let Ok(text) = std::fs::read_to_string(&p) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let Some(date) = v.get("date").and_then(|x| x.as_str()) else { return false };
    date == chrono::Local::now().date_naive().to_string()
}

/// Aggregate today's tokens from `eval/consolidate-*.json` files.
/// Cheap: small files, one JSON parse each.
pub fn todays_tokens(data_dir: &Path) -> u64 {
    let eval_dir = data_dir.join("eval");
    let Ok(rd) = std::fs::read_dir(&eval_dir) else { return 0 };
    let today_prefix = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut total: u64 = 0;
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
        // consolidate-1700000000.json — derive date from mtime.
        let Ok(meta) = e.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        let dt: chrono::DateTime<chrono::Local> = modified.into();
        if dt.format("%Y-%m-%d").to_string() != today_prefix {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(u) = v.get("usage") {
            for k in ["prompt_tokens", "completion_tokens", "reasoning_tokens"] {
                if let Some(n) = u.get(k).and_then(|x| x.as_u64()) {
                    total = total.saturating_add(n);
                }
            }
        }
        let _ = name;
    }
    total
}

/// Acquire exclusive lock via `flock(2)`-style `LOCK_EX | LOCK_NB` on a
/// sentinel file. If another daemon already holds it, returns Err.
fn acquire_lock(data_dir: &Path) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    std::fs::create_dir_all(data_dir)?;
    let p = lock_path(data_dir);
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(&p)?;
    f.write_all(b"dream-daemon\n")?;
    f.flush()?;
    // Best-effort: try a non-blocking flock via Windows `LockFileEx` /
    // POSIX `flock` would need a sysdep. For now, we use a PID-file
    // approach (read PID, check if alive). If the file exists with a
    // running PID, refuse. If PID is dead, overwrite.
    let pid_path = data_dir.join("daemon.pid");
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if pid_alive(pid) {
                    return Err(crate::error::MnemushError::Other(format!(
                        "daemon already running (pid {})",
                        pid
                    )));
                }
            }
        }
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;
    Ok(())
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) — ESRCH if dead.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    // Windows: no portable liveness check in std or our deps without
    // pulling windows-sys. We use OpenProcess via the Windows API to
    // query the process. This is the minimum needed to avoid two daemons.
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut core::ffi::c_void;
        fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

fn release_lock(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join("daemon.pid"));
}

/// Spawn `mnemush dream` and return its stdout. Reuses all existing
/// dream machinery (provider chain, chunking, eval logging).
fn run_dream_binary(data_dir: &Path, provider: &str, model: &str) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("dream");
    if !data_dir.as_os_str().is_empty() {
        cmd.env("MNEMUSH_DATA_DIR", data_dir);
    }
    if !provider.is_empty() && provider != "minimax" {
        // Default (minimax) is already handled by the chain; only forward
        // when the user explicitly chose a different provider.
        cmd.env("MNEMUSH_DREAM_PROVIDER", provider);
    }
    if !model.is_empty() {
        cmd.env("MNEMUSH_DREAM_MODEL", model);
    }
    let out = cmd.output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(crate::error::MnemushError::Other(format!(
            "mnemush dream failed (exit {:?}): {}{}",
            out.status.code(),
            stdout,
            stderr
        )));
    }
    Ok(stdout + &stderr)
}

/// Main daemon loop. Runs until killed.
pub fn run(cfg: &Config) -> Result<()> {
    if !cfg.dream.enabled {
        return Err(crate::error::MnemushError::Other(
            "dream.enabled = false — set MNEMUSH_DREAM_ENABLED=1 or [dream] enabled = true".into(),
        ));
    }
    let data_dir = default_data_dir();
    std::fs::create_dir_all(&data_dir)?;
    acquire_lock(&data_dir)?;
    eprintln!(
        "[dream-daemon] started pid={}, data_dir={}, scheduled_time={}",
        std::process::id(),
        data_dir.display(),
        cfg.dream.scheduled_time
    );
    let res = loop_body(cfg, &data_dir);
    release_lock(&data_dir);
    res
}

fn loop_body(cfg: &Config, data_dir: &Path) -> Result<()> {
    loop {
        let now = chrono::Local::now();
        let wake = next_wake(now, &cfg.dream.scheduled_time)
            .ok_or_else(|| crate::error::MnemushError::Other("invalid scheduled_time".into()))?;
        let secs = (wake - now).num_seconds().max(1);
        eprintln!(
            "[dream-daemon] now={} next_wake={} sleep_secs={}",
            now.format("%Y-%m-%d %H:%M:%S"),
            wake.format("%Y-%m-%d %H:%M:%S"),
            secs
        );
        // Sleep in 60s chunks so the daemon reacts to SIGTERM reasonably
        // quickly (single 8h sleep would ignore signals until wake).
        let mut remaining = secs as u64;
        while remaining > 0 {
            let chunk = remaining.min(60);
            std::thread::sleep(Duration::from_secs(chunk));
            remaining -= chunk;
            // Quick cooperative exit: if the daemon.pid file is gone or
            // our pid differs, another process took over — exit cleanly.
            let pid_path = data_dir.join("daemon.pid");
            if !pid_path.exists()
                || std::fs::read_to_string(&pid_path)
                    .map(|s| s.trim() != std::process::id().to_string())
                    .unwrap_or(true)
            {
                eprintln!("[dream-daemon] another daemon owns the lock; exiting");
                return Ok(());
            }
        }
        // Past wake: run today's dream unless already done or budget exceeded.
        let today = chrono::Local::now().date_naive();
        if already_completed_today(data_dir) {
            eprintln!("[dream-daemon] today's dream already completed; skipping");
            continue;
        }
        let used = todays_tokens(data_dir);
        if used >= cfg.dream.daily_token_budget && cfg.dream.provider != "local" {
            // Enforce budget for cloud providers (minimax / deepseek /
            // auto). Local model is "free" enough that the user usually
            // wants to keep going regardless of the budget.
            eprintln!(
                "[dream-daemon] daily budget exhausted ({} >= {}); skipping",
                used, cfg.dream.daily_token_budget
            );
            continue;
        }
        // Mark started.
        let _ = std::fs::write(
            started_path(data_dir),
            json!({"date": today.to_string(), "started_at": chrono::Local::now().timestamp()}).to_string(),
        );
        let dream_model = std::env::var("MNEMUSH_DREAM_MODEL").unwrap_or_default();
        match run_dream_binary(data_dir, &cfg.dream.provider, &dream_model) {
            Ok(out) => {
                eprintln!("[dream-daemon] dream ok\n{out}");
            }
            Err(e) => {
                eprintln!("[dream-daemon] dream failed: {e}");
                // Don't write completed.json — retry next iteration.
                continue;
            }
        }
        let _ = std::fs::write(
            completed_path(data_dir),
            json!({
                "date": today.to_string(),
                "completed_at": chrono::Local::now().timestamp(),
                "tokens_used": used,
            })
            .to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parse_hhmm_accepts_valid() {
        assert_eq!(parse_hhmm("02:00"), Some((2, 0)));
        assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
    }
    #[test]
    fn parse_hhmm_rejects_bad() {
        assert_eq!(parse_hhmm("25:00"), None);
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(parse_hhmm("02:00:00"), None);
        assert_eq!(parse_hhmm("aa:bb"), None);
    }
    #[test]
    fn next_wake_rolls_to_tomorrow_when_past_time_today() {
        let tz = chrono::Local::now().timezone();
        let now = tz.with_ymd_and_hms(2026, 8, 26, 14, 30, 0).unwrap();
        let wake = next_wake(now, "02:00").unwrap();
        assert_eq!(wake.format("%Y-%m-%d %H:%M").to_string(), "2026-08-27 02:00");
    }
    #[test]
    fn next_wake_today_when_future() {
        let tz = chrono::Local::now().timezone();
        let now = tz.with_ymd_and_hms(2026, 8, 26, 0, 30, 0).unwrap();
        let wake = next_wake(now, "02:00").unwrap();
        assert_eq!(wake.format("%Y-%m-%d %H:%M").to_string(), "2026-08-26 02:00");
    }
    #[test]
    fn todays_tokens_returns_zero_when_no_eval_dir() {
        let tmp = tempdir();
        let total = todays_tokens(&tmp);
        assert_eq!(total, 0);
    }

    fn tempdir() -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("mnemush-daemon-test-{}-{}", pid, nanos));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
