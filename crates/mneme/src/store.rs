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

use crate::error::{MnemeError, Result};
use crate::schema::{Category, Edge, EdgeType, Memory, MemoryType, Source, Tier};

const SCHEMA_VERSION: i32 = 2;

const SCHEMA_SQL: &str = r#"
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
    needs_review INTEGER NOT NULL DEFAULT 0
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
}

impl Store {
    /// Open (or create) the database at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // Sensible pragmas.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory database (used for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(SCHEMA_SQL)?;
        let current: Option<i32> = tx
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()?;
        let current = current.unwrap_or(0);
        if current < SCHEMA_VERSION {
            // Migration from v1 (with FTS5 triggers) to v2 (manual FTS).
            // Just bump version; existing FTS triggers on v1 dbs will be
            // ignored because we use manual sync.
            tx.execute(
                "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        } else if current > SCHEMA_VERSION {
            return Err(MnemeError::Other(format!(
                "schema version {} is newer than supported {}",
                current, SCHEMA_VERSION
            )));
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

    /// Begin a deferred transaction.
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        Ok(self.conn.transaction()?)
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
                content_hash, deleted_at, needs_review
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
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
            ],
        )?;
        // FTS5 needs INTEGER rowid. Use the memory table's internal
        // rowid (since `id` is TEXT, not aliased to rowid).
        let rowid: i64 = tx.query_row("SELECT last_insert_rowid()", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, context, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rowid, m.title, m.content, m.context, tags_str],
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

        Ok(Memory {
            id: row.get("id")?,
            memory_type: parse_memory_type(&memory_type_str),
            tier: parse_tier(&tier_str),
            category: parse_category(&category_str),
            title: row.get("title")?,
            content: row.get("content")?,
            context: row.get("context")?,
            topic_key: row.get("topic_key")?,
            tags,
            project: row.get("project")?,
            source: parse_source(&source_str),
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
        })
    }

    pub fn update_memory_tx(tx: &Transaction, m: &Memory) -> Result<()> {
        let tags_str = m.tags.join(" ");
        tx.execute(
            r#"UPDATE memory SET
                memory_type=?2, tier=?3, category=?4, title=?5, content=?6, context=?7,
                topic_key=?8, tags=?9, project=?10, source=?11,
                initial_confidence=?12, confidence=?13, importance=?14,
                access_count=?15, last_accessed_at=?16, created_at=?17,
                override_half_life=?18, never_prune=?19, never_decay=?20,
                content_hash=?21, deleted_at=?22, needs_review=?23
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
            ],
        )?;
        // Sync FTS: delete old rowid, insert new. Look up the integer
        // rowid from the memory table (since we joined on TEXT id).
        let rowid: i64 = tx.query_row(
            "SELECT rowid FROM memory WHERE id = ?1",
            params![m.id],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM memory_fts WHERE rowid = ?1", params![rowid])?;
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, context, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rowid, m.title, m.content, m.context, tags_str],
        )?;
        Ok(())
    }

    // ── Edge row operations ─────────────────────────────────────────

    pub fn insert_edge_tx(tx: &Transaction, e: &Edge) -> Result<()> {
        tx.execute(
            r#"INSERT INTO memory_edge (
                id, source_id, target_id, edge_type,
                strength, initial_strength, bidirectional,
                provenance, evidence, context,
                access_count, last_activated, stability,
                created_at, deleted_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
            )"#,
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
        Ok(())
    }

    pub fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<Edge> {
        let edge_type_str: String = row.get("edge_type")?;
        let last_act_ts: Option<i64> = row.get("last_activated")?;
        let created_ts: i64 = row.get("created_at")?;
        let deleted_ts: Option<i64> = row.get("deleted_at")?;
        let bidir: i32 = row.get("bidirectional")?;

        Ok(Edge {
            id: row.get("id")?,
            source_id: row.get("source_id")?,
            target_id: row.get("target_id")?,
            edge_type: parse_edge_type(&edge_type_str),
            strength: row.get("strength")?,
            initial_strength: row.get("initial_strength")?,
            bidirectional: bidir != 0,
            provenance: row.get("provenance")?,
            evidence: row.get("evidence")?,
            context: row.get("context")?,
            access_count: row.get("access_count")?,
            last_activated: last_act_ts.map(Self::ts_to_dt),
            stability: row.get("stability")?,
            created_at: Self::ts_to_dt(created_ts),
            deleted_at: deleted_ts.map(Self::ts_to_dt),
        })
    }

    // ── Audit log ───────────────────────────────────────────────────

    pub fn log_event(
        &self,
        event_type: &str,
        memory_id: Option<&str>,
        edge_id: Option<&str>,
        details: Option<&str>,
        actor: &str,
    ) -> Result<()> {
        self.conn.execute(
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

fn parse_memory_type(s: &str) -> MemoryType {
    match s {
        "identity" => MemoryType::Identity,
        "procedural" => MemoryType::Procedural,
        _ => MemoryType::Semantic,
    }
}

fn parse_tier(s: &str) -> Tier {
    match s {
        "project" => Tier::Project,
        "skill" => Tier::Skill,
        "session" => Tier::Session,
        _ => Tier::Global,
    }
}

fn parse_category(s: &str) -> Category {
    match s {
        "decision" => Category::Decision,
        "lesson" => Category::Lesson,
        "failure" => Category::Failure,
        "correction" => Category::Correction,
        "insight" => Category::Insight,
        "preference" => Category::Preference,
        "convention" => Category::Convention,
        "tool_quirk" => Category::ToolQuirk,
        "episodic" => Category::Episodic,
        "skill" => Category::Skill,
        "identity" => Category::Identity,
        _ => Category::Note,
    }
}

fn parse_source(s: &str) -> Source {
    match s {
        "auto_heuristic" => Source::AutoHeuristic,
        "auto_review" => Source::AutoReview,
        "correction" => Source::Correction,
        "skill" => Source::Skill,
        "session_import" => Source::SessionImport,
        "search_result" => Source::SearchResult,
        _ => Source::Manual,
    }
}

fn parse_edge_type(s: &str) -> EdgeType {
    match s {
        "supports" => EdgeType::Supports,
        "contradicts" => EdgeType::Contradicts,
        "supersedes" => EdgeType::Supersedes,
        _ => EdgeType::Related,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }
}
