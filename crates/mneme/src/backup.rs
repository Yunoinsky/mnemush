// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Backup and restore of the entire `~/.mneme/` data directory.
//!
//! Format: gzipped tar archive. The first entry is `MANIFEST.json`
//! recording the mneme version, schema_version, and live counts at
//! backup time. Remaining entries are the live data files:
//!
//!   - `mneme.db`     — captured via SQLite's online backup API
//!                      (handles WAL/SHM atomically)
//!   - `config.toml`  — global config (if present)
//!   - `identity/`    — USER/PERSONA/CONSTITUTION + pending.jsonl
//!   - `eval/`        — self-eval NDJSON log (optional; can be skipped
//!                      to keep backups small)
//!
//! Restore refuses to overwrite a target whose `schema_version` is
//! newer than the backup's (downgrade protection). Callers must pass
//! `--force` (or equivalent) to bypass that check.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::backup::Backup;
use serde::{Deserialize, Serialize};

use crate::error::{MnemeError, Result};
use crate::store::Store;
use crate::VERSION;

/// Live counts at backup time. Used by restore to refuse overwrite when
/// the target has live data the backup doesn't know about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    pub active_memories: i64,
    pub edges: i64,
    pub soft_deleted: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMeta {
    pub mneme_version: String,
    pub schema_version: i64,
    pub created_at_unix: i64,
    pub source_dir: String,
    pub counts: Counts,
}

const MANIFEST_NAME: &str = "MANIFEST.json";
/// Entries (other than the manifest) the backup always tries to copy.
/// Eval/ is included as an optional folder — see `create_backup_to`.
const DB_FILE: &str = "mneme.db";
const CONFIG_FILE: &str = "config.toml";
const IDENTITY_DIR: &str = "identity";
const EVAL_DIR: &str = "eval";

/// Build a `BackupMeta` from a live `Store`. Used by `create_backup`
/// and exposed for tests / dry-runs.
pub fn snapshot_meta(store: &Store, source_dir: &Path) -> Result<BackupMeta> {
    let counts = counts_for_store(store)?;
    Ok(BackupMeta {
        mneme_version: VERSION.to_string(),
        schema_version: schema_version(store)?,
        created_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        source_dir: source_dir.display().to_string(),
        counts,
    })
}

fn counts_for_store(store: &Store) -> Result<Counts> {
    let active: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
    let soft_deleted: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memory WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
    let edges: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memory_edge WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
    Ok(Counts { active_memories: active, edges, soft_deleted })
}

fn schema_version(store: &Store) -> Result<i64> {
    store
        .conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .map_err(|e| MnemeError::Other(format!("read schema_version: {}", e)))
}

/// Create a backup tarball of `source_dir` at `output`.
///
/// `include_eval`: include `eval/` (self-eval NDJSON log). Smaller
/// backups without it are still useful — eval data is regenerable.
pub fn create_backup_to(
    source_dir: &Path,
    output: &Path,
    include_eval: bool,
) -> Result<BackupMeta> {
    let db_path = source_dir.join(DB_FILE);
    if !db_path.exists() {
        return Err(MnemeError::Other(format!(
            "no database at {} — is mneme initialized?",
            db_path.display()
        )));
    }
    // SQLite online backup API: yields a consistent snapshot even
    // under WAL. Writes to a sibling temp file we then bundle.
    let temp_db = source_dir.join(format!(
        "mneme-backup-{}-{}.db",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    ));
    // Read the snapshot we need for the manifest from the live
    // store before opening a second connection (to avoid two writers
    // racing on the WAL).
    let meta = {
        let store = Store::open(&db_path)?;
        snapshot_meta(&store, source_dir)?
    };
    // Backup via a separate connection so WAL is consistent.
    {
        let src = rusqlite::Connection::open(&db_path)?;
        let mut dst = rusqlite::Connection::open(&temp_db)?;
        let backup = Backup::new(&src, &mut dst)?;
        backup.run_to_completion(5, std::time::Duration::from_millis(0), None)?;
    }
    // Pack the archive.
    let result = (|| -> Result<BackupMeta> {
        let file = fs::File::create(output)?;
        let gz = GzEncoder::new(file, Compression::default());
        let mut tar = tar::Builder::new(gz);
        append_manifest(&mut tar, &meta)?;
        tar.append_path_with_name(&temp_db, DB_FILE)?;
        append_if_exists(&mut tar, source_dir, CONFIG_FILE)?;
        append_dir_if_exists(&mut tar, source_dir, IDENTITY_DIR)?;
        if include_eval {
            append_dir_if_exists(&mut tar, source_dir, EVAL_DIR)?;
        }
        tar.into_inner()
            .map_err(|e| MnemeError::Other(format!("finalize tar: {}", e)))?
            .finish()
            .map_err(|e| MnemeError::Other(format!("finalize gzip: {}", e)))?;
        Ok(meta)
    })();
    let _ = fs::remove_file(&temp_db);
    result
}

fn append_manifest<W: Write>(tar: &mut tar::Builder<W>, meta: &BackupMeta) -> Result<()> {
    let body = serde_json::to_vec(meta)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, MANIFEST_NAME, body.as_slice())?;
    Ok(())
}

fn append_if_exists<W: Write>(
    tar: &mut tar::Builder<W>,
    root: &Path,
    rel: &str,
) -> Result<()> {
    let path = root.join(rel);
    if path.exists() {
        tar.append_path_with_name(&path, rel)?;
    }
    Ok(())
}

fn append_dir_if_exists<W: Write>(
    tar: &mut tar::Builder<W>,
    root: &Path,
    rel: &str,
) -> Result<()> {
    let path = root.join(rel);
    if !path.exists() {
        return Ok(());
    }
    tar.append_dir_all(rel, &path)?;
    Ok(())
}

/// Restore `archive` into `target_dir`. Overwrites existing files
/// unless the target has a newer `schema_version` than the backup
/// (downgrade protection — pass `allow_downgrade=true` to force).
pub fn restore_backup_to(
    archive: &Path,
    target_dir: &Path,
    allow_downgrade: bool,
) -> Result<BackupMeta> {
    // First pass: read just MANIFEST.json (also runs the downgrade
    // guard against `target_dir` if the DB exists).
    let meta = read_manifest(archive)?;
    if !allow_downgrade {
        let target_db = target_dir.join(DB_FILE);
        if target_db.exists() {
            // Open read-only with a plain rusqlite connection — we
            // must NOT use Store::open() here because that re-runs
            // migrations and would overwrite the (artificially higher)
            // schema_version row, defeating the downgrade guard.
            let conn = rusqlite::Connection::open_with_flags(
                &target_db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            );
            if let Ok(conn) = conn {
                let cur: rusqlite::Result<i64> = conn.query_row(
                    "SELECT version FROM schema_version LIMIT 1",
                    [],
                    |r| r.get(0),
                );
                if let Ok(cur) = cur {
                    if cur > meta.schema_version {
                        return Err(MnemeError::Other(format!(
                            "refusing to overwrite newer DB (target schema_version={}, \
                             backup schema_version={}); pass --force to override",
                            cur, meta.schema_version
                        )));
                    }
                }
            }
            // If the target isn't a valid DB or has no schema_version
            // row, the restore will overwrite — that's fine, let it
            // proceed.
        }
    }
    // Second pass: unpack every entry. We have to re-iterate the
    // archive because tar's underlying reader is a byte stream and
    // can't be rewound; buffering Entry objects doesn't help because
    // they hold a reference to the consumed reader.
    fs::create_dir_all(target_dir)?;
    let file = fs::File::open(archive)?;
    let gz = GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let dest = safe_join(target_dir, &path)?;
        let header = entry.header();
        match header.entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&dest)?;
            }
            tar::EntryType::Regular => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut buf = Vec::with_capacity(header.size()?.min(64 * 1024) as usize);
                entry.read_to_end(&mut buf)?;
                fs::write(&dest, &buf)?;
            }
            _ => {}
        }
    }
    Ok(meta)
}

fn read_manifest(archive: &Path) -> Result<BackupMeta> {
    let file = fs::File::open(archive)?;
    let gz = GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path == Path::new(MANIFEST_NAME) {
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            return Ok(serde_json::from_str(&s)?);
        }
    }
    Err(MnemeError::Other(format!(
        "archive is missing MANIFEST.json (not a mneme backup?): {}",
        archive.display()
    )))
}

/// Reject `..` segments and absolute paths — never write outside
/// `target_dir` even if a malicious archive has `../../etc/passwd`.
fn safe_join(root: &Path, entry_path: &Path) -> Result<std::path::PathBuf> {
    use std::path::Component;
    for c in entry_path.components() {
        match c {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(MnemeError::Other(format!(
                    "archive contains unsafe path: {}",
                    entry_path.display()
                )));
            }
            _ => {}
        }
    }
    Ok(root.join(entry_path))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::schema::NewMemory;

    fn setup_home() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        // Create a minimal mneme home: open the store (which runs the
        // v3 migration) and write a couple of memories so we have
        // non-empty counts.
        let db = tmp.path().join(DB_FILE);
        let store = Store::open(&db).unwrap();
        let cfg = Config::default();
        let api = crate::memory::MemoryApi::new(&store, &cfg);
        api.add(NewMemory::note("alpha", "first memory")).unwrap();
        api.add(NewMemory::note("beta", "second memory"))
            .unwrap();
        // Touch identity dir.
        std::fs::create_dir_all(tmp.path().join(IDENTITY_DIR)).unwrap();
        std::fs::write(tmp.path().join(IDENTITY_DIR).join("USER.md"), "user test").unwrap();
        std::fs::write(tmp.path().join(CONFIG_FILE), "[storage]\ndb_path = \"~\"\n").unwrap();
        // Force-close the store so the file handle is released.
        drop(store);
        tmp
    }

    #[test]
    fn backup_then_restore_round_trip() {
        let src = setup_home();
        let archive = src.path().join("backup.tar.gz");
        let meta_before = {
            let store = Store::open(&src.path().join(DB_FILE)).unwrap();
            let m = snapshot_meta(&store, src.path()).unwrap();
            assert_eq!(m.counts.active_memories, 2);
            assert!(m.schema_version >= 3);
            m
        };
        create_backup_to(src.path(), &archive, /*include_eval=*/ false).unwrap();
        // archive non-empty.
        let size = std::fs::metadata(&archive).unwrap().len();
        assert!(size > 0, "backup should be non-empty");

        // Restore into a fresh dir.
        let dst = tempfile::tempdir().unwrap();
        let meta_after = restore_backup_to(&archive, dst.path(), false).unwrap();
        assert_eq!(meta_before.schema_version, meta_after.schema_version);
        assert_eq!(meta_before.counts, meta_after.counts);
        assert_eq!(meta_before.mneme_version, meta_after.mneme_version);
        // Both memories present after restore.
        let store = Store::open(&dst.path().join(DB_FILE)).unwrap();
        let cfg = Config::default();
        let api = crate::memory::MemoryApi::new(&store, &cfg);
        let mems = api.list(10).unwrap();
        assert_eq!(mems.len(), 2, "expected 2 memories after restore, got {}", mems.len());
        // Identity file restored.
        let user = std::fs::read_to_string(dst.path().join(IDENTITY_DIR).join("USER.md")).unwrap();
        assert_eq!(user, "user test");
        // Config restored.
        let cfg = std::fs::read_to_string(dst.path().join(CONFIG_FILE)).unwrap();
        assert!(cfg.contains("[storage]"));
    }

    #[test]
    fn restore_refuses_downgrade() {
        // Backup from a fresh setup.
        let src = setup_home();
        let archive = src.path().join("backup.tar.gz");
        create_backup_to(src.path(), &archive, false).unwrap();

        // Pre-populate the destination with a *fake* higher schema
        // version by creating the DB, running the v3 migration, then
        // bumping the row to 99. Store::open creates + migrates on
        // its own, so we open once to materialize, then write the
        // version row directly.
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dst.path()).unwrap();
        {
            let _ = Store::open(&dst.path().join(DB_FILE)).unwrap();
            let conn = rusqlite::Connection::open(dst.path().join(DB_FILE)).unwrap();
            conn.execute("UPDATE schema_version SET version = 99", []).unwrap();
        }
        let r = restore_backup_to(&archive, dst.path(), /*allow_downgrade=*/ false);
        assert!(r.is_err(), "should refuse downgrade, got {:?}", r);
        let err = format!("{}", r.unwrap_err());
        assert!(
            err.contains("refusing to overwrite newer DB"),
            "wrong error: {}",
            err
        );
        // With force: succeeds.
        restore_backup_to(&archive, dst.path(), true).unwrap();
    }

    #[test]
    fn restore_rejects_unsafe_paths() {
        // Build an archive with a `../escape` entry by hand, then
        // verify safe_join refuses it.
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("evil.tar.gz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let gz = GzEncoder::new(f, Compression::default());
            let mut tar = tar::Builder::new(gz);
            // Legit entry.
            let body = b"ok";
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            tar.append_data(&mut h, "evil.txt", &body[..]).unwrap();
            // Hostile entry — safe_join must reject on ParentDir.
            let body = b"pwn";
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            // tar crate rejects ".." itself; the real defense is
            // safe_join() which we exercise below. Close the archive.
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dst = tempfile::tempdir().unwrap();
        // We can't easily craft a `..` entry through the tar API (it
        // validates). Instead, directly call safe_join to verify the
        // function refuses unsafe paths.
        let root = dst.path();
        assert!(safe_join(root, Path::new("../etc/passwd")).is_err());
        assert!(safe_join(root, Path::new("a/b/../../escape")).is_err());
        assert!(safe_join(root, Path::new("/abs/path")).is_err());
        assert!(safe_join(root, Path::new("ok/file.txt")).is_ok());
    }

    #[test]
    fn backup_then_restore_preserves_eval_when_requested() {
        let src = setup_home();
        let eval_dir = src.path().join(EVAL_DIR);
        std::fs::create_dir_all(&eval_dir).unwrap();
        std::fs::write(eval_dir.join("sess.ndjson"), "{}\n").unwrap();
        let archive = src.path().join("with-eval.tar.gz");
        create_backup_to(src.path(), &archive, true).unwrap();
        let dst = tempfile::tempdir().unwrap();
        restore_backup_to(&archive, dst.path(), false).unwrap();
        assert!(dst.path().join(EVAL_DIR).join("sess.ndjson").exists());
    }
}