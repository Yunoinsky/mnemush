//! SQLite storage layer.
//!
//! Owns the `rusqlite::Connection`, schema migrations, FTS5 sync, and
//! the low-level row <-> struct conversion. Higher-level operations
//! (memory add/search/get, edge link/neighbors) live in [`memory`] /
//! [`edge`].
//!
//! Note: FTS5 is synced manually from Rust rather than via SQL triggers
//! because FTS5 triggers on `content=` external tables mis-parse
//! parentheses in user content. Manually doing `INSERT INTO fts(rowid,
//! title, content, context, tags) VALUES (...)` with bound parameters
//! avoids the issue.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::error::{MnemushError, Result};
use crate::schema::{Category, Edge, Memory, MemoryType, Source, Tier};

pub(super) const SCHEMA_VERSION: i32 = 5;

pub(super) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS memory (
    id TEXT PRIMARY KEY,
    memory_type TEXT NOT NULL,
    tier TEXT NOT NULL,
    category TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    context TEXT,
    topic_key TEXT,
    tags TEXT NOT NULL DEFAULT '',
    project TEXT,
    source TEXT NOT NULL,

    initial_confidence REAL NOT NULL DEFAULT 1.0,
    confidence REAL NOT NULL DEFAULT 1.0,
    importance REAL NOT NULL DEFAULT 0.5,

    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,

    override_half_life REAL,
    never_prune INTEGER NOT NULL DEFAULT 0,
    never_decay INTEGER NOT NULL DEFAULT 0,

    content_hash TEXT NOT NULL,
    deleted_at INTEGER,
    needs_review INTEGER NOT NULL DEFAULT 0,

    status TEXT NOT NULL DEFAULT 'active',
    due_at INTEGER,
    claimed_by TEXT,
    parent_id TEXT,
    completed_at INTEGER,

    -- v1.6.2: device id of the first creator. NULL for memories
    -- migrated from v4 (use `mnemush memory reorigin` to backfill).
    origin_device TEXT
);

CREATE INDEX IF NOT EXISTS idx_memory_type ON memory(memory_type);
CREATE INDEX IF NOT EXISTS idx_memory_category ON memory(category);
CREATE INDEX IF NOT EXISTS idx_memory_topic ON memory(topic_key);
CREATE INDEX IF NOT EXISTS idx_memory_project ON memory(project);
CREATE INDEX IF NOT EXISTS idx_memory_created ON memory(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_memory_hash ON memory(content_hash);
CREATE INDEX IF NOT EXISTS idx_memory_active ON memory(deleted_at) WHERE deleted_at IS NULL;

-- Standalone FTS5 index (not external-content). Synced manually from
-- Rust. Default unicode61 tokenizer (categories=1) which treats
-- hyphens as token separators.
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    title, content, context, tags,
    tokenize = 'unicode61 remove_diacritics 1'
);

CREATE TABLE IF NOT EXISTS memory_edge (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    strength REAL NOT NULL DEFAULT 0.5,
    initial_strength REAL NOT NULL DEFAULT 0.5,
    bidirectional INTEGER NOT NULL DEFAULT 0,
    provenance TEXT,
    evidence TEXT,
    context TEXT,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_activated INTEGER,
    stability REAL NOT NULL DEFAULT 7.0,
    created_at INTEGER NOT NULL,
    deleted_at INTEGER,
    UNIQUE(source_id, target_id, edge_type),
    FOREIGN KEY (source_id) REFERENCES memory(id) ON DELETE CASCADE,
    FOREIGN KEY (target_id) REFERENCES memory(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_edge_source ON memory_edge(source_id);
CREATE INDEX IF NOT EXISTS idx_edge_target ON memory_edge(target_id);
CREATE INDEX IF NOT EXISTS idx_edge_type ON memory_edge(edge_type);
CREATE INDEX IF NOT EXISTS idx_edge_active ON memory_edge(deleted_at) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS memory_event (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    memory_id TEXT,
    edge_id TEXT,
    details TEXT,
    actor TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_event_memory ON memory_event(memory_id);
CREATE INDEX IF NOT EXISTS idx_event_created ON memory_event(created_at DESC);
"#;

/// A handle to the SQLite database.
pub struct Store {
    pub conn: Connection,
    /// On-disk path (None for in-memory/test stores).
    pub db_path: Option<std::path::PathBuf>,
}

impl Store {
    /// Open (or create) the database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // Sensible pragmas.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // 多连接场景(webdav 自动同步的异步 push 线程重开连接并发写)下,
        // 遇到写锁等待而非立即 SQLITE_BUSY。
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        let mut store = Self {
            conn,
            db_path: Some(path.to_path_buf()),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory database (used for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self {
            conn,
            db_path: None,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        // Step 1: ensure the canonical schema (CREATE TABLE IF NOT
        // EXISTS) is in place. For a fresh DB this is enough; for an
        // existing DB it is a no-op.
        let tx = self.conn.transaction()?;
        tx.execute_batch(SCHEMA_SQL)?;

        // Step 2: read the current schema_version. None = never
        // initialized (treat as fresh); Some(v > SCHEMA_VERSION) =
        // refuse to open (forward-compat guard); Some(v) = run the
        // migrations whose target_version > v in order.
        let current: Option<i32> = tx
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()?;
        match current {
            None => {
                // Fresh DB: run the full registry so every table
                // (including ones added by later migrations, e.g.
                // memory_embedding from V3ToV4) exists. This was a
                // bug in v0.4: the None arm inserted SCHEMA_VERSION
                // directly, skipping the registry, so a fresh DB
                // never got tables created by non-first migrations.
                // Fresh DB: the schema_version table is empty, so the
                // first migration's INSERT creates the row and each
                // subsequent UPDATE overwrites it. (INSERT OR REPLACE
                // would accumulate rows because `version` is the PK —
                // different version values never conflict, so the
                // SELECT later reads the first = the oldest.)
                let mut first = true;
                for m in crate::migrations::default_registry() {
                    m.up(&tx).map_err(|e| {
                        MnemushError::Other(format!(
                            "fresh-db migration to v{}: {}",
                            m.target_version(),
                            e
                        ))
                    })?;
                    if first {
                        tx.execute(
                            "INSERT INTO schema_version (version) VALUES (?1)",
                            params![m.target_version()],
                        )?;
                        first = false;
                    } else {
                        tx.execute(
                            "UPDATE schema_version SET version = ?1",
                            params![m.target_version()],
                        )?;
                    }
                }
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(MnemushError::Other(format!(
                    "schema version {} is newer than supported {}",
                    v, SCHEMA_VERSION
                )));
            }
            Some(v) => {
                // Walk the registry from low to high. Each migration
                // bumps schema_version, so subsequent migrations see
                // a fresh view. Migrations are idempotent (use
                // pragma_table_info guards) so re-running on a half-
                // migrated DB is safe.
                for m in crate::migrations::default_registry() {
                    if m.target_version() <= v as i64 {
                        continue;
                    }
                    m.up(&tx).map_err(|e| {
                        MnemushError::Other(format!("migration to v{}: {}", m.target_version(), e))
                    })?;
                    tx.execute(
                        "UPDATE schema_version SET version = ?1",
                        params![m.target_version()],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn now_ts() -> i64 {
        Utc::now().timestamp()
    }

    pub fn ts_to_dt(ts: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
    }

    /// Same as `ts_to_dt` but returns Option for nullable timestamps.
    pub fn ts_to_dt_opt(ts: i64) -> Option<DateTime<Utc>> {
        Utc.timestamp_opt(ts, 0).single()
    }

    /// Begin an unchecked (non-blocking) transaction.
    pub fn unchecked_transaction(&mut self) -> Result<Transaction<'_>> {
        Ok(self.conn.unchecked_transaction()?)
    }

    // ── Memory row operations ──────────────────────────────────────

    pub fn insert_memory_tx(tx: &Transaction, m: &Memory) -> Result<()> {
        let tags_str = m.tags.join(" ");
        tx.execute(
            r#"INSERT INTO memory (
                id, memory_type, tier, category, title, content, context,
                topic_key, tags, project, source,
                initial_confidence, confidence, importance,
                access_count, last_accessed_at, created_at,
                override_half_life, never_prune, never_decay,
                content_hash, deleted_at, needs_review,
                status, due_at, claimed_by, parent_id, completed_at,
                origin_device
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27, ?28, ?29
            )"#,
            params![
                m.id,
                m.memory_type.as_str(),
                m.tier.as_str(),
                m.category.as_str(),
                m.title,
                m.content,
                m.context,
                m.topic_key,
                tags_str,
                m.project,
                m.source.as_str(),
                m.initial_confidence,
                m.confidence,
                m.importance,
                m.access_count,
                m.last_accessed_at.timestamp(),
                m.created_at.timestamp(),
                m.override_half_life,
                m.never_prune as i32,
                m.never_decay as i32,
                m.content_hash,
                m.deleted_at.map(|d| d.timestamp()),
                m.needs_review as i32,
                m.status.as_str(),
                m.due_at.map(|d| d.timestamp()),
                &m.claimed_by,
                &m.parent_id,
                m.completed_at.map(|d| d.timestamp()),
                &m.origin_device,
            ],
        )?;
        // FTS5 gets its own auto-assigned rowid (omit `rowid` column).
        // Earlier code reused `memory.rowid` from `last_insert_rowid()`,
        // which silently collided with orphan FTS5 rows left behind by
        // partial cleanup of the memory table, surfacing as
        // "constraint failed" at insert time.
        tx.execute(
            "INSERT INTO memory_fts(title, content, context, tags) VALUES (?1, ?2, ?3, ?4)",
            params![m.title, m.content, m.context, tags_str],
        )?;
        Ok(())
    }

    /// Delete a memory's FTS row by its memory-table rowid (same tx).
    /// Keeps the standalone FTS index rowid-aligned with `memory` after
    /// soft-delete / merge.
    pub fn delete_fts_for_tx(tx: &Transaction, id: &str) -> Result<()> {
        tx.execute(
            "DELETE FROM memory_fts WHERE rowid = (SELECT rowid FROM memory WHERE id = ?1)",
            params![id],
        )?;
        Ok(())
    }

    pub fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<Memory> {
        let memory_type_str: String = row.get("memory_type")?;
        let tier_str: String = row.get("tier")?;
        let category_str: String = row.get("category")?;
        let source_str: String = row.get("source")?;
        let tags_str: String = row.get("tags")?;
        let tags: Vec<String> = if tags_str.is_empty() {
            vec![]
        } else {
            tags_str.split(' ').map(String::from).collect()
        };

        let last_ts: i64 = row.get("last_accessed_at")?;
        let created_ts: i64 = row.get("created_at")?;
        let deleted_ts: Option<i64> = row.get("deleted_at")?;
        let never_prune_i: i32 = row.get("never_prune")?;
        let never_decay_i: i32 = row.get("never_decay")?;
        let needs_review_i: i32 = row.get("needs_review")?;
        let status_str: String = row.get("status")?;
        let due_at_ts: Option<i64> = row.get("due_at")?;
        let claimed_by: Option<String> = row.get("claimed_by")?;
        let parent_id: Option<String> = row.get("parent_id")?;
        let completed_at_ts: Option<i64> = row.get("completed_at")?;

        Ok(Memory {
            id: row.get("id")?,
            memory_type: parse_memory_type(&memory_type_str)?,
            tier: parse_tier(&tier_str)?,
            category: parse_category(&category_str)?,
            title: row.get("title")?,
            content: row.get("content")?,
            context: row.get("context")?,
            topic_key: row.get("topic_key")?,
            tags,
            project: row.get("project")?,
            source: parse_source(&source_str)?,
            initial_confidence: row.get("initial_confidence")?,
            confidence: row.get("confidence")?,
            importance: row.get("importance")?,
            access_count: row.get("access_count")?,
            last_accessed_at: Self::ts_to_dt(last_ts),
            created_at: Self::ts_to_dt(created_ts),
            override_half_life: row.get("override_half_life")?,
            never_prune: never_prune_i != 0,
            never_decay: never_decay_i != 0,
            content_hash: row.get("content_hash")?,
            deleted_at: deleted_ts.map(Self::ts_to_dt),
            needs_review: needs_review_i != 0,
            status: crate::schema::ActionStatus::parse(&status_str)
                .unwrap_or(crate::schema::ActionStatus::Active),
            due_at: due_at_ts.and_then(Self::ts_to_dt_opt),
            claimed_by,
            parent_id,
            completed_at: completed_at_ts.and_then(Self::ts_to_dt_opt),
            origin_device: row.get("origin_device")?,
        })
    }

    pub fn update_memory_tx(tx: &Transaction, m: &Memory) -> Result<()> {
        // Only updates the memory row. FTS5 sync intentionally omitted:
        // the only caller in v0.1 (`MemoryApi::search` access-boost) only
        // mutates `confidence`, `last_accessed_at`, `access_count` —
        // nothing FTS5 indexes. If a future caller mutates title/content,
        // FTS5 sync needs to come back (see insert_memory_tx for pattern).
        tx.execute(
            r#"UPDATE memory SET
                memory_type=?2, tier=?3, category=?4, title=?5, content=?6, context=?7,
                topic_key=?8, tags=?9, project=?10, source=?11,
                initial_confidence=?12, confidence=?13, importance=?14,
                access_count=?15, last_accessed_at=?16, created_at=?17,
                override_half_life=?18, never_prune=?19, never_decay=?20,
                content_hash=?21, deleted_at=?22, needs_review=?23,
                status=?24, due_at=?25, claimed_by=?26, parent_id=?27, completed_at=?28,
                origin_device=?29
              WHERE id=?1"#,
            params![
                m.id,
                m.memory_type.as_str(),
                m.tier.as_str(),
                m.category.as_str(),
                m.title,
                m.content,
                m.context,
                m.topic_key,
                m.tags.join(" "),
                m.project,
                m.source.as_str(),
                m.initial_confidence,
                m.confidence,
                m.importance,
                m.access_count,
                m.last_accessed_at.timestamp(),
                m.created_at.timestamp(),
                m.override_half_life,
                m.never_prune as i32,
                m.never_decay as i32,
                m.content_hash,
                m.deleted_at.map(|d| d.timestamp()),
                m.needs_review as i32,
                m.status.as_str(),
                m.due_at.map(|d| d.timestamp()),
                &m.claimed_by,
                &m.parent_id,
                m.completed_at.map(|d| d.timestamp()),
                &m.origin_device,
            ],
        )?;
        Ok(())
    }

    // ── Audit log ───────────────────────────────────────────────────

    /// Read an `Edge` row from a `memory_edge` SELECT. Public so
    /// `crate::sync` (cross-machine export) can use it without
    /// exposing the row-tuple format.
    pub fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<Edge> {
        let edge_type_str: String = row.get("edge_type")?;
        let access_count_i: i32 = row.get("access_count")?;
        let bidirectional_i: i32 = row.get("bidirectional")?;
        let last_activated_ts: Option<i64> = row.get("last_activated")?;
        let created_at_ts: i64 = row.get("created_at")?;
        let deleted_at_ts: Option<i64> = row.get("deleted_at")?;
        Ok(Edge {
            id: row.get("id")?,
            source_id: row.get("source_id")?,
            target_id: row.get("target_id")?,
            edge_type: crate::schema::EdgeType::parse(&edge_type_str)
                .unwrap_or(crate::schema::EdgeType::Related),
            strength: row.get("strength")?,
            initial_strength: row.get("initial_strength")?,
            bidirectional: bidirectional_i != 0,
            provenance: row.get("provenance")?,
            evidence: row.get("evidence")?,
            context: row.get("context")?,
            access_count: access_count_i.max(0) as u32,
            last_activated: last_activated_ts.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
            stability: row.get("stability")?,
            created_at: chrono::DateTime::from_timestamp(created_at_ts, 0)
                .unwrap_or_else(chrono::Utc::now),
            deleted_at: deleted_at_ts.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        })
    }

    pub fn log_event(
        &self,
        event_type: &str,
        memory_id: Option<&str>,
        edge_id: Option<&str>,
        details: Option<&str>,
        actor: &str,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.log_event_tx(&tx, event_type, memory_id, edge_id, details, actor)?;
        tx.commit()?;
        Ok(())
    }

    /// Same as `log_event` but participates in the caller's transaction.
    /// Used by `forget::prune_apply` and `forget::isolate_hard_delete` so
    /// the soft-delete + audit row commit atomically.
    pub fn log_event_tx(
        &self,
        tx: &Transaction,
        event_type: &str,
        memory_id: Option<&str>,
        edge_id: Option<&str>,
        details: Option<&str>,
        actor: &str,
    ) -> Result<()> {
        tx.execute(
            r#"INSERT INTO memory_event (id, event_type, memory_id, edge_id, details, actor, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
            params![
                uuid::Uuid::new_v4().to_string(),
                event_type,
                memory_id,
                edge_id,
                details,
                actor,
                Self::now_ts(),
            ],
        )?;
        Ok(())
    }
}

/// String → enum parser. Unknown values are an error, not a silent
/// fallback. Use this for round-tripping DB rows; writes already go
/// through the strict `Enum::parse()` (returns `Option`) so the
/// only way an unknown value lands in the DB is direct SQL or a
/// future-schema mismatch — both should fail loudly at read time.
macro_rules! parse_enum {
    ($s:expr, $enum:ty, { $($variant:ident => $str:expr),+ $(,)? }) => {
        match $s {
            $($str => Ok(<$enum>::$variant),)+
            _ => Err($crate::error::MnemushError::Invalid(
                format!("unknown {}: '{}'", stringify!($enum), $s),
            )),
        }
    };
}

fn parse_memory_type(s: &str) -> Result<MemoryType> {
    parse_enum!(s, MemoryType, {
        Identity => "identity",
        Procedural => "procedural",
        Semantic => "semantic",
    })
}

fn parse_tier(s: &str) -> Result<Tier> {
    parse_enum!(s, Tier, {
        Global => "global",
        Project => "project",
        Skill => "skill",
        Session => "session",
    })
}

fn parse_category(s: &str) -> Result<Category> {
    parse_enum!(s, Category, {
        Decision => "decision",
        Lesson => "lesson",
        Failure => "failure",
        Correction => "correction",
        Insight => "insight",
        Preference => "preference",
        Convention => "convention",
        ToolQuirk => "tool_quirk",
        ForgetTrace => "forget_trace",
        Episodic => "episodic",
        Skill => "skill",
        Identity => "identity",
        Note => "note",
    })
}

fn parse_source(s: &str) -> Result<Source> {
    parse_enum!(s, Source, {
        Manual => "manual",
        AutoHeuristic => "auto_heuristic",
        AutoReview => "auto_review",
        Correction => "correction",
        Skill => "skill",
        SessionImport => "session_import",
        SearchResult => "search_result",
        FileTree => "file_tree",
        Consolidate => "consolidate",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ActionStatus;

    #[test]
    fn opens_in_memory() {
        let store = Store::open_in_memory().unwrap();
        let v: i32 = store
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn opens_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let _ = Store::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn round_trip_memory_row() {
        let store = Store::open_in_memory().unwrap();
        let m = sample_memory();
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();

        let got: Memory = store
            .conn
            .query_row(
                "SELECT * FROM memory WHERE id=?1",
                params![m.id],
                Store::row_to_memory,
            )
            .unwrap();
        assert_eq!(got.id, m.id);
        assert_eq!(got.title, m.title);
        assert_eq!(got.tags, m.tags);
    }

    #[test]
    fn unknown_category_errors_instead_of_silent_fallback() {
        // Insert a row with a hand-written bogus category that
        // bypasses the strict Category::parse path. Loading it
        // should now error loudly, not silently coerce to Note.
        // Regression: error message must clearly identify the
        // unknown value — NOT the misleading "Conversion error
        // from type Text at index: 0" wrapper.
        let store = Store::open_in_memory().unwrap();
        let bogus_id = uuid::Uuid::new_v4().to_string();
        store.conn.execute(
            "INSERT INTO memory (id, memory_type, tier, category, title, content, tags, source, \
             initial_confidence, confidence, importance, access_count, last_accessed_at, created_at, \
             never_prune, never_decay, content_hash, needs_review) \
             VALUES (?1, 'semantic', 'global', 'decizion', 't', 'c', '', 'manual', \
             1.0, 1.0, 0.5, 0, 0, 0, 0, 0, 'h', 0)",
            params![bogus_id],
        ).unwrap();

        let r = store.conn.query_row(
            "SELECT * FROM memory WHERE id=?1",
            params![bogus_id],
            Store::row_to_memory,
        );
        let err = r.unwrap_err();
        // Error must clearly identify the unknown category.
        let mut src: &dyn std::error::Error = &err;
        let mut found = false;
        while let Some(s) = src.source() {
            let msg = s.to_string();
            if msg.contains("decizion") || msg.contains("Category") {
                found = true;
                break;
            }
            src = s;
        }
        assert!(
            found,
            "expected error mentioning 'decizion' or 'Category', got: {err}"
        );
        // Must NOT use the misleading generic wrapper.
        assert!(
            !err.to_string().contains("Conversion error from type Text"),
            "error should not use misleading FromSqlConversionFailure wrapper, got: {err}"
        );
    }

    #[test]
    fn unknown_tier_errors_without_misleading_wrapper() {
        // Regression test for the user's bug: tier='active' was a
        // real value in their DB (added during external-wiki session
        // before tier validation). Reading such rows must surface a
        // clean MnemushError, not 'Conversion error from type Text'.
        let store = Store::open_in_memory().unwrap();
        let bogus_id = uuid::Uuid::new_v4().to_string();
        store.conn.execute(
            "INSERT INTO memory (id, memory_type, tier, category, title, content, tags, source, \
             initial_confidence, confidence, importance, access_count, last_accessed_at, created_at, \
             never_prune, never_decay, content_hash, needs_review) \
             VALUES (?1, 'semantic', 'active', 'note', 't', 'c', '', 'manual', \
             1.0, 1.0, 0.5, 0, 0, 0, 0, 0, 'h', 0)",
            params![bogus_id],
        ).unwrap();

        let r = store.conn.query_row(
            "SELECT * FROM memory WHERE id=?1",
            params![bogus_id],
            Store::row_to_memory,
        );
        let err = r.unwrap_err();
        // Must mention 'active' or 'Tier' somewhere in source chain
        let mut src: &dyn std::error::Error = &err;
        let mut found = false;
        while let Some(s) = src.source() {
            let msg = s.to_string();
            if msg.contains("active") || msg.contains("Tier") {
                found = true;
                break;
            }
            src = s;
        }
        assert!(found, "expected 'active' or 'Tier' in error chain: {err}");
        assert!(
            !err.to_string().contains("Conversion error from type Text"),
            "error must not use misleading Conversion wrapper, got: {err}"
        );
    }

    #[test]
    fn half_migrated_db_recovers_without_duplicate_column_error() {
        // Regression: v0.2 → v0.3 migration used to crash with
        // "duplicate column name: status" when re-run on a DB that
        // already had the v3 columns (e.g. schema_version stayed at 2
        // but ALTERs had been applied). Fix: detect via
        // pragma_table_info before each ADD COLUMN, and update
        // schema_version after each migration arm.
        //
        // Setup: write a v0.2-shaped memory table (no status/etc.),
        // set schema_version=2, then manually add the v3 columns to
        // simulate a crash between ALTER and UPDATE schema_version.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("half.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
            INSERT INTO schema_version (version) VALUES (2);
            CREATE TABLE memory (
                id TEXT PRIMARY KEY,
                memory_type TEXT NOT NULL, tier TEXT NOT NULL,
                category TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL,
                context TEXT, topic_key TEXT, tags TEXT NOT NULL DEFAULT '',
                project TEXT, source TEXT NOT NULL,
                initial_confidence REAL NOT NULL DEFAULT 1.0,
                confidence REAL NOT NULL DEFAULT 1.0,
                importance REAL NOT NULL DEFAULT 0.5,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed_at INTEGER NOT NULL, created_at INTEGER NOT NULL,
                override_half_life REAL,
                never_prune INTEGER NOT NULL DEFAULT 0,
                never_decay INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT NOT NULL,
                deleted_at INTEGER, needs_review INTEGER NOT NULL DEFAULT 0
            );
        "#,
        )
        .unwrap();
        // Simulate the crash: ALTERs ran, but schema_version not bumped.
        conn.execute_batch("ALTER TABLE memory ADD COLUMN status TEXT NOT NULL DEFAULT 'active';")
            .unwrap();
        conn.execute_batch("ALTER TABLE memory ADD COLUMN due_at INTEGER;")
            .unwrap();
        conn.execute_batch("ALTER TABLE memory ADD COLUMN claimed_by TEXT;")
            .unwrap();
        conn.execute_batch("ALTER TABLE memory ADD COLUMN parent_id TEXT;")
            .unwrap();
        conn.execute_batch("ALTER TABLE memory ADD COLUMN completed_at INTEGER;")
            .unwrap();
        drop(conn);
        // Now open via Store::open: must NOT crash on duplicate-column.
        let store = Store::open(&path).unwrap();
        let v: i32 = store
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            v, SCHEMA_VERSION,
            "schema_version should advance to {}",
            SCHEMA_VERSION
        );
        // And v0.3 columns are still there.
        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('memory') WHERE name='status'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "status column should still be present");
    }

    #[test]
    fn fts5_handles_parens() {
        let store = Store::open_in_memory().unwrap();
        let mut m = sample_memory();
        m.content = "test (with parens) and other stuff".into();
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
        // search should work without FTS5 parse error
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_fts WHERE memory_fts MATCH ?1",
                params!["parens"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "should find the parens content via FTS5");
    }

    fn sample_memory() -> Memory {
        use chrono::Utc;
        Memory {
            id: uuid::Uuid::new_v4().to_string(),
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            category: Category::Note,
            title: "test".into(),
            content: "test content".into(),
            context: None,
            topic_key: Some("test".into()),
            tags: vec!["a".into(), "b".into()],
            project: None,
            source: Source::Manual,
            initial_confidence: 1.0,
            confidence: 1.0,
            importance: 0.5,
            access_count: 0,
            last_accessed_at: Utc::now(),
            created_at: Utc::now(),
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            content_hash: "abc".into(),
            deleted_at: None,
            needs_review: false,
            status: ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
            origin_device: None,
        }
    }
}
