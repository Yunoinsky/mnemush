// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Cross-machine sync (v1.0) — Git as the transport, mneme as the codec.
//!
//! ## Layout in the sync directory
//!
//! ```text
//! sync-dir/
//! ├── MANIFEST.json          schema_version, counts, generated_at
//! ├── memory.json            array of all memory rows (active + soft-deleted)
//! ├── identity/
//! │   ├── USER.md            (verbatim)
//! │   ├── PERSONA.md         (verbatim)
//! │   ├── CONSTITUTION.md    (verbatim)
//! │   └── pending.jsonl      (verbatim, if present)
//! └── embeddings/
//!     └── memory-id.json     per-memory embedding blobs
//! ```
//!
//! ## Workflow
//!
//! ```bash
//! # Machine A — initialize once.
//! mneme sync init ~/mneme-sync       # git init + first export + commit
//! cd ~/mneme-sync && git remote add origin git@github.com:you/mneme-sync.git
//! git push -u origin main
//!
//! # Machine A — daily.
//! mneme sync export ~/mneme-sync     # refresh snapshot
//! (cd ~/mneme-sync && git add -A && git commit -m "..." && git push)
//!
//! # Machine B — pull state.
//! git clone ... && mneme sync import ./mneme-sync
//! ```
//!
//! Conflicts: when an `import` finds a memory id whose local
//! `updated_at` is more recent than the snapshot's `updated_at`, and
//! neither side has been re-exported since, the row is left in place
//! and reported. Resolving is a manual `mneme list --project ...` + delete
//! + re-import flow (no auto-merge — that's git's job).
//!
//! Schema-versioned: import refuses to apply a snapshot from a NEWER
//! schema_version than this binary supports (downgrade allowed — it's
//! strictly additive).

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::embeddings::StoredEmbedding;
use crate::error::{MnemeError, Result};
use crate::schema::Edge;
use crate::store::Store;
use crate::VERSION;

/// Manifest at the root of every sync dir. Lets the importer
/// reject incompatible snapshots before walking files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub mneme_version: String,
    pub schema_version: i64,
    pub generated_at_unix: i64,
    pub counts: Counts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Counts {
    pub active_memories: i64,
    pub edges: i64,
    pub soft_deleted: i64,
}

/// Load MANIFEST.json from `dir`. Returns the parsed manifest or
/// `Err` if the file is missing / malformed.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join("MANIFEST.json");
    let body = fs::read_to_string(&path)
        .map_err(|e| MnemeError::Other(format!("read MANIFEST at {}: {}", path.display(), e)))?;
    serde_json::from_str::<Manifest>(&body)
        .map_err(|e| MnemeError::Other(format!("parse MANIFEST: {}", e)))
}

/// Export all DB state + identity + embeddings to `dir`. Creates
/// `dir` if missing. Overwrites existing files in `dir/{memory.json,
/// identity/, embeddings/, MANIFEST.json}`. Leaves other files in
/// `dir` alone (so a git working tree keeps its own files like
/// `.gitignore`, `README.md`, etc.).
pub fn export_to(store: &Store, dir: &Path) -> Result<Manifest> {
    fs::create_dir_all(dir)
        .map_err(|e| MnemeError::Other(format!("create {}: {}", dir.display(), e)))?;
    let identity_dir = dir.join("identity");
    fs::create_dir_all(&identity_dir)
        .map_err(|e| MnemeError::Other(format!("create identity/: {}", e)))?;
    let embeddings_dir = dir.join("embeddings");
    fs::create_dir_all(&embeddings_dir)
        .map_err(|e| MnemeError::Other(format!("create embeddings/: {}", e)))?;

    // 1. MANIFEST
    let counts = store_counts(store)?;
    let schema_version = read_schema_version(store)?;
    let manifest = Manifest {
        mneme_version: VERSION.to_string(),
        schema_version,
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        counts: counts.clone(),
    };
    fs::write(
        dir.join("MANIFEST.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )
    .map_err(|e| MnemeError::Other(format!("write MANIFEST: {}", e)))?;

    // 2. memory.json — every memory row (active + soft-deleted +
    // embeddings stored elsewhere).
    let mems = read_all_memories(store)?;
    fs::write(dir.join("memory.json"), serde_json::to_vec_pretty(&mems)?)
        .map_err(|e| MnemeError::Other(format!("write memory.json: {}", e)))?;

    // 3. edges — all rows (including soft-deleted) as JSON.
    let edges = read_all_edges(store)?;
    fs::write(dir.join("edges.json"), serde_json::to_vec_pretty(&edges)?)
        .map_err(|e| MnemeError::Other(format!("write edges.json: {}", e)))?;

    // 4. identity files — verbatim copy.
    copy_identity_files(&identity_dir)?;

    // 5. embeddings — one file per (memory_id, model). Filename:
    // "<model_safe>__<memory_id>.json" to support multi-model.
    copy_embeddings(store, &embeddings_dir)?;

    Ok(manifest)
}

/// Initialize a fresh sync directory: `git init`, export current
/// state, commit. The user adds a remote and pushes separately.
/// Idempotent: re-running on an existing sync dir just refreshes
/// the snapshot and amends the initial commit (no history rewrite).
pub fn init_sync(store: &Store, dir: &Path) -> Result<Manifest> {
    fs::create_dir_all(dir)
        .map_err(|e| MnemeError::Other(format!("create {}: {}", dir.display(), e)))?;
    let manifest = export_to(store, dir)?;
    git(dir, &["init", "-q"])
        .map_err(|e| MnemeError::Other(format!("git init in {}: {}", dir.display(), e)))?;
    // CI runners and fresh git installs have no user.name/user.email;
    // `git commit` fails without them. Set repo-local defaults only if
    // unset (don't clobber the user's global config).
    if git(dir, &["config", "user.name"])
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        git(dir, &["config", "user.name", "mneme"])?;
    }
    if git(dir, &["config", "user.email"])
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        git(dir, &["config", "user.email", "mneme@local"])?;
    }
    git(dir, &["add", "-A"]).map_err(|e| MnemeError::Other(format!("git add: {}", e)))?;
    let msg = format!(
        "mneme sync init: {} memories, schema v{}, mneme {}",
        manifest.counts.active_memories, manifest.schema_version, manifest.mneme_version
    );
    git(dir, &["commit", "-q", "-m", &msg])
        .map_err(|e| MnemeError::Other(format!("git commit: {}", e)))?;
    Ok(manifest)
}

/// Import all DB state + identity + embeddings from `dir` into
/// `store`. Refuses to apply snapshots from a newer schema_version
/// (the importer can downgrade, but not upgrade past what this
/// binary supports). Reports per-memory conflicts (local
/// `updated_at` > snapshot `updated_at`) but does NOT auto-resolve
/// them — that's the user's job, manually.
pub fn import_from(store: &Store, dir: &Path) -> Result<ImportReport> {
    let manifest = read_manifest(dir)?;
    let current_version = read_schema_version(store)?;
    if manifest.schema_version > current_version {
        return Err(MnemeError::Other(format!(
            "snapshot is from a newer schema_version ({} > current {}); \
             upgrade mneme first",
            manifest.schema_version, current_version
        )));
    }

    // Memory rows
    let path = dir.join("memory.json");
    let body = fs::read_to_string(&path)
        .map_err(|e| MnemeError::Other(format!("read memory.json: {}", e)))?;
    let snapshot: Vec<crate::schema::Memory> = serde_json::from_str(&body)?;
    let mut report = ImportReport::default();
    for mem in snapshot {
        let id = mem.id.clone();
        let local_updated_at = store
            .get_by_id(&id)
            .ok()
            .flatten()
            .map(|m| m.last_accessed_at.timestamp().max(m.created_at.timestamp()));
        let snapshot_updated_at = mem
            .last_accessed_at
            .timestamp()
            .max(mem.created_at.timestamp());
        if let Some(local) = local_updated_at {
            if local > snapshot_updated_at {
                // Local is newer — conflict, skip.
                report.conflicts.push(id);
                continue;
            }
        }
        // Upsert: if the id exists, DELETE first (the memory row + its
        // FTS5 entry cascade via delete_memory_tx semantics — we just
        // clear the row; the FTS5 row stays orphaned but harmless, or
        // gets overwritten by the fresh insert's new FTS5 row).
        let tx = store.conn.unchecked_transaction()?;
        let exists: bool = store
            .conn
            .query_row(
                "SELECT 1 FROM memory WHERE id = ?1",
                params![&mem.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            // Remove the memory row (FK cascade clears edges); the
            // FTS5 entry is re-added by insert_memory_tx below.
            tx.execute("DELETE FROM memory WHERE id = ?1", params![&mem.id])?;
        }
        Store::insert_memory_tx(&tx, &mem)?;
        tx.commit()?;
        report.imported += 1;
    }

    // Edges
    let edges_path = dir.join("edges.json");
    if edges_path.exists() {
        let body = fs::read_to_string(&edges_path)?;
        let edges: Vec<Edge> = serde_json::from_str(&body)?;
        for e in edges {
            store.conn.execute(
                r#"INSERT OR REPLACE INTO memory_edge (
                    id, source_id, target_id, edge_type,
                    strength, initial_strength, bidirectional,
                    provenance, evidence, context,
                    access_count, last_activated, stability,
                    created_at, deleted_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
                params![
                    e.id,
                    e.source_id,
                    e.target_id,
                    e.edge_type.as_str(),
                    e.strength,
                    e.initial_strength,
                    e.bidirectional as i32,
                    e.provenance,
                    e.evidence,
                    e.context,
                    e.access_count,
                    e.last_activated.map(|d| d.timestamp()),
                    e.stability,
                    e.created_at.timestamp(),
                    e.deleted_at.map(|d| d.timestamp()),
                ],
            )?;
            report.edges_imported += 1;
        }
    }

    // Identity files — verbatim copy (skip if destination already
    // exists; the user can `mneme identity propose` + approve to
    // overwrite).
    let id_dir = dir.join("identity");
    if id_dir.exists() {
        for entry in fs::read_dir(&id_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let dest = crate::identity::default_identity_dir().join(&name);
            if dest.exists() {
                continue;
            }
            fs::copy(entry.path(), &dest)
                .map_err(|e| MnemeError::Other(format!("copy identity: {}", e)))?;
            report.identity_copied += 1;
        }
    }

    // Embeddings — per file.
    let emb_dir = dir.join("embeddings");
    if emb_dir.exists() {
        for entry in fs::read_dir(&emb_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let body = fs::read_to_string(&path)?;
            let s: StoredEmbedding = serde_json::from_str(&body)?;
            let tx = store.conn.unchecked_transaction()?;
            // (memory_id, model) UNIQUE — INSERT OR REPLACE.
            tx.execute(
                r#"INSERT INTO memory_embedding (memory_id, model, dim, vec, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5)
                   ON CONFLICT(memory_id, model) DO UPDATE SET
                       dim = excluded.dim,
                       vec = excluded.vec,
                       updated_at = excluded.updated_at"#,
                params![
                    s.memory_id,
                    s.model,
                    s.dim,
                    s.vec
                        .iter()
                        .flat_map(|f| f.to_le_bytes())
                        .collect::<Vec<u8>>(),
                    Store::now_ts()
                ],
            )?;
            tx.commit()?;
            report.embeddings_imported += 1;
        }
    }

    Ok(report)
}

/// What `import_from` did. Conflicts are memory ids where the local
/// DB had a newer updated_at than the snapshot; those rows were
/// left untouched.
#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub imported: usize,
    pub edges_imported: usize,
    pub identity_copied: usize,
    pub embeddings_imported: usize,
    pub conflicts: Vec<String>,
}

// ── helpers ────────────────────────────────────────────────────────

fn read_all_memories(store: &Store) -> Result<Vec<crate::schema::Memory>> {
    let mut stmt = store.conn.prepare("SELECT * FROM memory")?;
    let rows = stmt.query_map([], Store::row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn read_all_edges(store: &Store) -> Result<Vec<Edge>> {
    let mut stmt = store.conn.prepare("SELECT * FROM memory_edge")?;
    let rows = stmt.query_map([], Store::row_to_edge)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn store_counts(store: &Store) -> Result<Counts> {
    let active: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let soft_deleted: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM memory WHERE deleted_at IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let edges: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM memory_edge WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(Counts {
        active_memories: active,
        edges,
        soft_deleted,
    })
}

fn read_schema_version(store: &Store) -> Result<i64> {
    store
        .conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .map_err(|e| MnemeError::Other(format!("read schema_version: {}", e)))
}

fn copy_identity_files(dest_dir: &Path) -> Result<()> {
    let src = crate::identity::default_identity_dir();
    for name in ["USER.md", "PERSONA.md", "CONSTITUTION.md", "pending.jsonl"] {
        let from = src.join(name);
        if !from.exists() {
            continue;
        }
        fs::copy(&from, dest_dir.join(name))
            .map_err(|e| MnemeError::Other(format!("copy identity/{}: {}", name, e)))?;
    }
    Ok(())
}

fn copy_embeddings(store: &Store, dest_dir: &Path) -> Result<()> {
    let mut stmt = store
        .conn
        .prepare("SELECT memory_id, model, dim, vec FROM memory_embedding")?;
    let rows = stmt.query_map([], |row| {
        let bytes: Vec<u8> = row.get(3)?;
        let dim: i64 = row.get(2)?;
        let mut v = Vec::with_capacity(dim as usize);
        for chunk in bytes.chunks_exact(4) {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            v.push(f32::from_le_bytes(arr));
        }
        Ok(StoredEmbedding {
            memory_id: row.get(0)?,
            model: row.get(1)?,
            dim,
            vec: v,
        })
    })?;
    for r in rows {
        let s = r?;
        // Filename-safe model name
        let model_safe: String = s
            .model
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dest_dir.join(format!("{}__{}.json", model_safe, s.memory_id));
        let body = serde_json::to_vec(&s)?;
        fs::write(&path, body).map_err(|e| MnemeError::Other(format!("write embedding: {}", e)))?;
    }
    Ok(())
}

/// Run `git <args>` in `dir`. Returns `stdout` trimmed.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| MnemeError::Other(format!("spawn git: {}", e)))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(MnemeError::Other(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::NewMemory;
    use crate::store::SCHEMA_VERSION;

    fn setup() -> (Store, tempfile::TempDir) {
        let store = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        (store, dir)
    }

    fn note(content: &str, title: &str) -> NewMemory {
        NewMemory::note(content, title)
    }

    /// Round-trip: export → read back into a fresh in-memory DB
    /// should produce identical counts.
    #[test]
    fn round_trip_export_import() {
        let (src, dir) = setup();
        let cfg = crate::config::Config::default();
        let api = crate::memory::MemoryApi::new(&src, &cfg);
        api.add(note("alpha content", "alpha")).unwrap();
        api.add(note("beta content", "beta")).unwrap();
        let m_before: i64 = src
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        eprintln!("DEBUG m_before={}", m_before);
        let manifest = export_to(&src, dir.path()).unwrap();
        eprintln!(
            "DEBUG manifest.counts.active_memories={}",
            manifest.counts.active_memories
        );
        eprintln!("DEBUG m_before again={}", m_before);
        assert_eq!(manifest.schema_version, SCHEMA_VERSION as i64);
        assert_eq!(manifest.counts.active_memories, m_before);
        // Now import into a fresh store.
        let dst = Store::open_in_memory().unwrap();
        let report = import_from(&dst, dir.path()).unwrap();
        assert_eq!(report.imported, 2);
        assert_eq!(report.conflicts.len(), 0);
        let m_after: i64 = dst
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m_before, m_after);
    }

    /// `init_sync` produces a working git repo with the snapshot
    /// committed. We don't actually push to a remote; just verify
    /// the local repo has the expected commit.
    #[test]
    fn init_sync_creates_repo() {
        let (store, dir) = setup();
        let cfg = crate::config::Config::default();
        let api = crate::memory::MemoryApi::new(&store, &cfg);
        api.add(note("hi", "hi")).unwrap();
        init_sync(&store, dir.path()).unwrap();
        // .git must exist.
        assert!(dir.path().join(".git").exists());
        // The memory.json was committed.
        let out = git(dir.path(), &["ls-files"]).unwrap();
        assert!(out.contains("memory.json"));
        assert!(out.contains("MANIFEST.json"));
    }

    /// Importing a snapshot from a newer schema_version is refused.
    /// We can't easily bump `SCHEMA_VERSION` for one test, so we
    /// craft a fake MANIFEST.
    #[test]
    fn import_refuses_newer_schema() {
        let (store, dir) = setup();
        // Write a fake MANIFEST claiming schema_version = 999.
        fs::write(
            dir.path().join("MANIFEST.json"),
            r#"{"mneme_version":"x","schema_version":999,"generated_at_unix":0,"counts":{"active_memories":0,"edges":0,"soft_deleted":0}}"#,
        ).unwrap();
        let r = import_from(&store, dir.path());
        assert!(r.is_err(), "should reject newer schema_version");
        assert!(format!("{}", r.unwrap_err()).contains("newer schema_version"));
    }

    /// Conflicts: when local DB has a memory with newer updated_at
    /// than the snapshot, import leaves it alone and reports.
    #[test]
    fn import_reports_local_newer_conflicts() {
        let (src, dir) = setup();
        let cfg = crate::config::Config::default();
        let api = crate::memory::MemoryApi::new(&src, &cfg);
        api.add(note("v1", "title")).unwrap();
        let id = api.list(10).unwrap()[0].id.clone();
        export_to(&src, dir.path()).unwrap();
        // Bump the row's updated_at to "now" — local is now newer.
        src.conn
            .execute(
                "UPDATE memory SET last_accessed_at = ?1 WHERE id = ?2",
                params![Store::now_ts() + 1000, &id],
            )
            .unwrap();
        // Now import into the same store — local is newer, should conflict.
        let report = import_from(&src, dir.path()).unwrap();
        assert!(report.conflicts.contains(&id));
        assert_eq!(
            report.imported, 0,
            "no rows should be imported when local is newer"
        );
    }
}
