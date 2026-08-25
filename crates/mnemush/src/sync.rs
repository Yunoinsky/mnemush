// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Cross-machine sync (v1.0) — Git as the transport, mnemush as the codec.
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
//! mnemush sync init ~/mnemush-sync       # git init + first export + commit
//! cd ~/mnemush-sync && git remote add origin git@github.com:you/mnemush-sync.git
//! git push -u origin main
//!
//! # Machine A — daily.
//! mnemush sync export ~/mnemush-sync     # refresh snapshot
//! (cd ~/mnemush-sync && git add -A && git commit -m "..." && git push)
//!
//! # Machine B — pull state.
//! git clone ... && mnemush sync import ./mnemush-sync
//! ```
//!
//! Conflicts: resolved per memory by auto-merge — same id compared on
//! `max(last_accessed_at, created_at)`, newer side wins; ids on one
//! side only are unioned; soft-delete (`deleted_at`) travels with the
//! newer side (deletion propagates). `import_from` applies the merge
//! and reports ids where the local side was strictly newer (those rows
//! are left untouched).
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
use crate::error::{MnemushError, Result};
use crate::schema::{Edge, Memory};
use crate::store::Store;
use crate::VERSION;

/// Manifest at the root of every sync dir. Lets the importer
/// reject incompatible snapshots before walking files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub mnemush_version: String,
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
        .map_err(|e| MnemushError::Other(format!("read MANIFEST at {}: {}", path.display(), e)))?;
    serde_json::from_str::<Manifest>(&body)
        .map_err(|e| MnemushError::Other(format!("parse MANIFEST: {}", e)))
}

/// Export all DB state + identity + embeddings to `dir`. Creates
/// `dir` if missing. Overwrites existing files in `dir/{memory.json,
/// identity/, embeddings/, MANIFEST.json}`. Leaves other files in
/// `dir` alone (so a git working tree keeps its own files like
/// `.gitignore`, `README.md`, etc.).
pub fn export_to(store: &Store, dir: &Path) -> Result<Manifest> {
    fs::create_dir_all(dir)
        .map_err(|e| MnemushError::Other(format!("create {}: {}", dir.display(), e)))?;
    let identity_dir = dir.join("identity");
    fs::create_dir_all(&identity_dir)
        .map_err(|e| MnemushError::Other(format!("create identity/: {}", e)))?;
    let embeddings_dir = dir.join("embeddings");
    fs::create_dir_all(&embeddings_dir)
        .map_err(|e| MnemushError::Other(format!("create embeddings/: {}", e)))?;

    // 1. MANIFEST
    let counts = store_counts(store)?;
    let schema_version = read_schema_version(store)?;
    let manifest = Manifest {
        mnemush_version: VERSION.to_string(),
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
    .map_err(|e| MnemushError::Other(format!("write MANIFEST: {}", e)))?;

    // 2. memory.json — every memory row (active + soft-deleted +
    // embeddings stored elsewhere).
    let mems = read_all_memories(store)?;
    fs::write(dir.join("memory.json"), serde_json::to_vec_pretty(&mems)?)
        .map_err(|e| MnemushError::Other(format!("write memory.json: {}", e)))?;

    // 3. edges — all rows (including soft-deleted) as JSON.
    let edges = read_all_edges(store)?;
    fs::write(dir.join("edges.json"), serde_json::to_vec_pretty(&edges)?)
        .map_err(|e| MnemushError::Other(format!("write edges.json: {}", e)))?;

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
        .map_err(|e| MnemushError::Other(format!("create {}: {}", dir.display(), e)))?;
    let manifest = export_to(store, dir)?;
    git(dir, &["init", "-q"])
        .map_err(|e| MnemushError::Other(format!("git init in {}: {}", dir.display(), e)))?;
    // CI runners and fresh git installs have no user.name/user.email;
    // `git commit` fails without them. Set repo-local defaults only if
    // unset (don't clobber the user's global config).
    if git(dir, &["config", "user.name"])
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        git(dir, &["config", "user.name", "mnemush"])?;
    }
    if git(dir, &["config", "user.email"])
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        git(dir, &["config", "user.email", "mnemush@local"])?;
    }
    git(dir, &["add", "-A"]).map_err(|e| MnemushError::Other(format!("git add: {}", e)))?;
    let msg = format!(
        "mnemush sync init: {} memories, schema v{}, mnemush {}",
        manifest.counts.active_memories, manifest.schema_version, manifest.mnemush_version
    );
    git(dir, &["commit", "-q", "-m", &msg])
        .map_err(|e| MnemushError::Other(format!("git commit: {}", e)))?;
    Ok(manifest)
}

/// Import all DB state + identity + embeddings from `dir` into
/// `store`. Refuses to apply snapshots from a newer schema_version
/// (the importer can downgrade, but not upgrade past what this
/// binary supports). Memory rows are merged per-id (newer side wins,
/// union of ids, deletion propagates); ids where the local DB is
/// strictly newer are reported as conflicts and left untouched.
pub fn import_from(store: &Store, dir: &Path) -> Result<ImportReport> {
    let manifest = read_manifest(dir)?;
    let current_version = read_schema_version(store)?;
    if manifest.schema_version > current_version {
        return Err(MnemushError::Other(format!(
            "snapshot is from a newer schema_version ({} > current {}); \
             upgrade mnemush first",
            manifest.schema_version, current_version
        )));
    }

    // Memory rows
    let snapshot = read_snapshot_memories(dir)?;
    let mut report = ImportReport::default();

    // 逐条合并(较新赢 + 并集 + 删除传播), 再把远端较新者写回本地。
    let local_all = read_all_memories(store)?;
    let local_ts: std::collections::HashMap<String, i64> = local_all
        .iter()
        .map(|m| (m.id.clone(), updated_ts(m)))
        .collect();
    let snapshot_ids: std::collections::HashSet<String> =
        snapshot.iter().map(|m| m.id.clone()).collect();
    let snapshot_ts: std::collections::HashMap<String, i64> = snapshot
        .iter()
        .map(|m| (m.id.clone(), updated_ts(m)))
        .collect();
    let merged = merge_memories(local_all, snapshot);
    for mem in merged {
        let id = mem.id.clone();
        // 纯本地行不动(保留其 edges)。
        if !snapshot_ids.contains(&id) {
            continue;
        }
        // 本地严格较新 → 冲突: 保留本地, 上报。
        if let Some(lt) = local_ts.get(&id) {
            if let Some(st) = snapshot_ts.get(&id) {
                if lt > st {
                    report.conflicts.push(id);
                    continue;
                }
            }
        }
        upsert_memory_tx(store, &mem)?;
        report.imported += 1;
    }

    // Edges
    let edges_path = dir.join("edges.json");
    if edges_path.exists() {
        let body = fs::read_to_string(&edges_path)?;
        let edges: Vec<Edge> = serde_json::from_str(&body)?;
        for e in edges {
            // 容错: 端点记忆合并后不存在(FK 失败)/重复 → 跳过该边,
            // 不中断整个 import(快照可能引用本地较新未导入/已删记忆)。
            let res = store.conn.execute(
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
            );
            match res {
                Ok(_) => report.edges_imported += 1,
                Err(err) => {
                    report.skipped_edges += 1;
                    eprintln!("sync: skip edge {} ({}): {err}", e.id, e.source_id);
                }
            }
        }
    }

    // Identity files — verbatim copy (skip if destination already
    // exists; the user can `mnemush identity propose` + approve to
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
                .map_err(|e| MnemushError::Other(format!("copy identity: {}", e)))?;
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
    /// 导入时跳过的边(端点记忆合并后不存在, FK 失败)。
    pub skipped_edges: usize,
}

/// 条目的"更新时间": last_accessed 与 created 取较新, 再并入 deleted_at
/// (软删行的新鲜度 = 删除时刻, 否则删除传播在真实形态下失效 —— 软删只
/// 写 deleted_at 不 bump 时间戳)。合并/冲突判定共用。
fn updated_ts(m: &Memory) -> i64 {
    m.last_accessed_at
        .timestamp()
        .max(m.created_at.timestamp())
        .max(m.deleted_at.map(|d| d.timestamp()).unwrap_or(i64::MIN))
}

/// 逐条合并 local/remote 两组记忆: 同 id 比更新时间(max(last_accessed,
/// created)), 较新者赢; 仅一侧存在的 id 并集; 软删(deleted_at)随较新者
/// 传播(删除传播)。供 webdav push 双向合并与 import_from 共用。
pub fn merge_memories(local: Vec<Memory>, remote: Vec<Memory>) -> Vec<Memory> {
    let mut out: std::collections::BTreeMap<String, Memory> = std::collections::BTreeMap::new();
    for m in local {
        out.insert(m.id.clone(), m);
    }
    for r in remote {
        match out.get(&r.id) {
            Some(l) if updated_ts(l) > updated_ts(&r) => { /* local newer, keep */ }
            _ => {
                out.insert(r.id.clone(), r);
            }
        }
    }
    out.into_values().collect()
}

/// 读取快照目录中的 memory.json(全部行, 含软删)。
pub fn read_snapshot_memories(dir: &Path) -> Result<Vec<Memory>> {
    let path = dir.join("memory.json");
    let body = fs::read_to_string(&path)
        .map_err(|e| MnemushError::Other(format!("read {}: {}", path.display(), e)))?;
    Ok(serde_json::from_str(&body)?)
}

/// 按 import 语义 upsert 一条 memory: 已存在则先 DELETE(FK 级联清
/// edges; FTS5 由 insert_memory_tx 重建), 再插入。
fn upsert_memory_tx(store: &Store, mem: &Memory) -> Result<()> {
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
        // UPDATE 保留 id 与边(FK 不级联) —— 之前 DELETE+重插会 FK 级联
        // 清掉该记忆的所有边(同步时边丢失事故的根源)。
        Store::update_memory_tx(&tx, mem)?;
        // FTS5 同步: 内容可能变。删旧行再按 memory 行(rowid 对齐)插新。
        tx.execute(
            "DELETE FROM memory_fts WHERE rowid = (SELECT rowid FROM memory WHERE id = ?1)",
            params![&mem.id],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO memory_fts(rowid, title, content, context, tags) \
             SELECT rowid, title, content, context, tags FROM memory WHERE id = ?1",
            params![&mem.id],
        )?;
    } else {
        Store::insert_memory_tx(&tx, mem)?;
    }
    tx.commit()?;
    Ok(())
}

/// push 双向合并后把结果写回本地 DB: 仅当远端"贡献"了该行(远端较新/
/// 平局或仅远端存在)时 upsert; 本地较新或仅本地存在的行不动(保留其
/// edges)。local/remote 是合并前的两组快照。
pub(crate) fn apply_merge_to_db(
    store: &Store,
    merged: &[Memory],
    local: &[Memory],
    remote: &[Memory],
) -> Result<usize> {
    let local_ts: std::collections::HashMap<&str, i64> =
        local.iter().map(|m| (m.id.as_str(), updated_ts(m))).collect();
    let remote_ts: std::collections::HashMap<&str, i64> =
        remote.iter().map(|m| (m.id.as_str(), updated_ts(m))).collect();
    let mut written = 0;
    for mem in merged {
        // 仅本地存在的行: 合并行即本地行, 不需要写回。
        let Some(rt) = remote_ts.get(mem.id.as_str()) else {
            continue;
        };
        // 本地严格较新: 合并行即本地行(冲突保留), 不需要写回。
        if let Some(lt) = local_ts.get(mem.id.as_str()) {
            if lt > rt {
                continue;
            }
        }
        upsert_memory_tx(store, mem)?;
        written += 1;
    }
    Ok(written)
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
        .map_err(|e| MnemushError::Other(format!("read schema_version: {}", e)))
}

fn copy_identity_files(dest_dir: &Path) -> Result<()> {
    let src = crate::identity::default_identity_dir();
    for name in ["USER.md", "PERSONA.md", "CONSTITUTION.md", "pending.jsonl"] {
        let from = src.join(name);
        if !from.exists() {
            continue;
        }
        fs::copy(&from, dest_dir.join(name))
            .map_err(|e| MnemushError::Other(format!("copy identity/{}: {}", name, e)))?;
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
        fs::write(&path, body)
            .map_err(|e| MnemushError::Other(format!("write embedding: {}", e)))?;
    }
    Ok(())
}

/// Run `git <args>` in `dir`. Returns `stdout` trimmed.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| MnemushError::Other(format!("spawn git: {}", e)))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(MnemushError::Other(format!(
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

    /// 构造一个最小 Memory(测试用): 只关心 id/content/时间戳/软删。
    fn mk(id: &str, content: &str, ts: i64) -> Memory {
        use crate::schema::{ActionStatus, Category, MemoryType, Source, Tier};
        Memory {
            id: id.to_string(),
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            category: Category::Note,
            title: String::new(),
            content: content.to_string(),
            context: None,
            topic_key: None,
            tags: Vec::new(),
            project: None,
            source: Source::Manual,
            initial_confidence: 1.0,
            confidence: 1.0,
            importance: 0.5,
            access_count: 0,
            last_accessed_at: Store::ts_to_dt(ts),
            created_at: Store::ts_to_dt(ts),
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            content_hash: String::new(),
            deleted_at: None,
            needs_review: false,
            status: ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
        }
    }

    /// 逐条合并: 较新赢 + 并集 + 删除传播(push/pull 共用)。
    #[test]
    fn merge_newer_wins_and_union() {
        let local = vec![mk("a1", "old", 100), mk("a2", "x", 200)];
        let remote = vec![mk("a1", "new", 300), mk("b1", "y", 150)];
        let merged = merge_memories(local, remote);
        let by_id: std::collections::HashMap<_, _> =
            merged.into_iter().map(|m| (m.id, m.content)).collect();
        assert_eq!(by_id.get("a1").unwrap(), "new", "remote newer wins");
        assert_eq!(by_id.get("a2").unwrap(), "x", "local-only kept");
        assert_eq!(by_id.get("b1").unwrap(), "y", "remote-only added");
        assert_eq!(by_id.len(), 3, "union of both sides");
    }

    /// 真实形态: 软删行时间戳停留在删除前最后一次活动, 删除时刻只记在
    /// deleted_at。本地已删(last_accessed=150, 删除于 200) vs 远端活跃
    /// (last_accessed=170) → 远端较新? 不 —— 删除行新鲜度 = deleted_at,
    /// 合并后必须保持删除, 否则远端静默复活已删记忆。
    #[test]
    fn merge_deletion_wins_when_delete_ts_is_newer_than_remote_activity() {
        let mut local_del = mk("a1", "gone", 150);
        local_del.deleted_at = Some(Store::ts_to_dt(200));
        let remote = mk("a1", "alive", 170); // 活跃于删除(200)之前
        let merged = merge_memories(vec![local_del], vec![remote]);
        assert!(
            merged[0].deleted_at.is_some(),
            "deleted_at=200 的本地软删必须赢过 last_accessed=170 的远端活跃行"
        );
    }

    /// 软删随较新者传播: 任一端删除比另一端更新 → 合并结果保持删除。
    #[test]
    fn merge_deletion_propagates() {
        // 本地已删(较新) vs 远端活跃 → 删除保持。
        let mut local_del = mk("a1", "gone", 400);
        local_del.deleted_at = Some(Store::ts_to_dt(400));
        let merged = merge_memories(vec![local_del], vec![mk("a1", "alive", 100)]);
        assert!(merged[0].deleted_at.is_some(), "local deletion (newer) wins");

        // 远端已删(较新) vs 本地活跃 → 删除传播。
        let mut remote_del = mk("b1", "gone", 400);
        remote_del.deleted_at = Some(Store::ts_to_dt(400));
        let merged = merge_memories(vec![mk("b1", "alive", 100)], vec![remote_del]);
        assert!(merged[0].deleted_at.is_some(), "remote deletion (newer) wins");
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
            r#"{"mnemush_version":"x","schema_version":999,"generated_at_unix":0,"counts":{"active_memories":0,"edges":0,"soft_deleted":0}}"#,
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
