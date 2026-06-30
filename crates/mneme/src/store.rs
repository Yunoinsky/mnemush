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
use crate::schema::{Category, Memory, MemoryType, Source, Tier};

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
        // v0.1 ships the current schema on a fresh DB. Older schema
        // versions don't exist yet (v1+ will be added when a real
        // migration is needed). If a future binary sees a newer
        // schema_version, refuse to open to avoid silent corruption.
        let tx = self.conn.transaction()?;
        tx.execute_batch(SCHEMA_SQL)?;
        let current: Option<i32> = tx
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()?;
        match current {
            None => {
                tx.execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![SCHEMA_VERSION],
                )?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(MnemeError::Other(format!(
                    "schema version {} is newer than supported {}",
                    v, SCHEMA_VERSION
                )));
            }
            Some(_) => {
                // v0.1 schema: nothing to migrate. Future versions
                // will add `if v < X { /* upgrade */ }` arms here.
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
            ],
        )?;
        Ok(())
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

/// String → enum parser. Unknown values are an error, not a silent
/// fallback. Use this for round-tripping DB rows; writes already go
/// through the strict `Enum::parse()` (returns `Option`) so the
/// only way an unknown value lands in the DB is direct SQL or a
/// future-schema mismatch — both should fail loudly at read time.
macro_rules! parse_enum {
    ($s:expr, $enum:ty, { $($variant:ident => $str:expr),+ $(,)? }) => {
        match $s {
            $($str => Ok(<$enum>::$variant),)+
            _ => Err($crate::error::MnemeError::Invalid(
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
    })
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
    fn unknown_category_errors_instead_of_silent_fallback() {
        // Insert a row with a hand-written bogus category that
        // bypasses the strict Category::parse path. Loading it
        // should now error loudly, not silently coerce to Note.
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
        // The original MnemeError is preserved inside the rusqlite
        // FromSqlConversionFailure; walk the source chain to assert.
        let mut src: &dyn std::error::Error = &err;
        let mut found = false;
        while let Some(s) = src.source() {
            if s.to_string().contains("unknown Category: 'decizion'") {
                found = true;
                break;
            }
            src = s;
        }
        assert!(found, "expected parse error mentioning 'decizion', got: {err}");
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
