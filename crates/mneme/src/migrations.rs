// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Schema migrations: trait + registry.
//!
//! Each version bump is a `Migration` impl. The store collects them
//! in the registry, runs them in order until reaching
//! `SCHEMA_VERSION`, and bumps `schema_version` after each.
//!
//! Adding a v0.3 → v0.4 migration: write a new struct impl'ing
//! `Migration`, append it to `default_registry()`. No changes to
//! `Store::migrate` needed.

use rusqlite::{params, Transaction};

use crate::error::{MnemeError, Result};

/// A single schema upgrade step.
///
/// `up(tx)` runs within the store's transaction. Implementations
/// MUST be idempotent — re-running on a half-migrated DB (where a
/// prior binary crash left schema_version stale relative to actual
/// columns) must be a no-op, not a duplicate-column error. Use
/// `pragma_table_info` / `pragma_index_info` checks before DDL.
pub trait Migration: Send + Sync {
    /// Run this migration on `tx`. After this returns Ok, the store
    /// updates `schema_version` to `target_version()` and proceeds
    /// to the next migration.
    fn up(&self, tx: &Transaction) -> Result<()>;

    /// The schema version this migration produces. Must be unique
    /// across all registered migrations and strictly increasing
    /// relative to previous ones.
    fn target_version(&self) -> i64;
}

/// v0.1 → v0.2: FTS5 rowid auto-assignment + add `source`,
/// `initial_confidence`, `confidence`, `importance`, `access_count`,
/// `last_accessed_at`, `override_half_life`, `never_prune`,
/// `never_decay`, `needs_review`. Plus the `idx_memory_active`
/// partial index. The old v0.1 shape lacked these; the new shape
/// adds them with sensible defaults — no data movement needed.
pub struct V1ToV2;
impl Migration for V1ToV2 {
    fn target_version(&self) -> i64 {
        2
    }
    fn up(&self, tx: &Transaction) -> Result<()> {
        let alters: &[&str] = &[
            "ALTER TABLE memory ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';",
            "ALTER TABLE memory ADD COLUMN initial_confidence REAL NOT NULL DEFAULT 1.0;",
            "ALTER TABLE memory ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;",
            "ALTER TABLE memory ADD COLUMN importance REAL NOT NULL DEFAULT 0.5;",
            "ALTER TABLE memory ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE memory ADD COLUMN last_accessed_at INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE memory ADD COLUMN override_half_life REAL;",
            "ALTER TABLE memory ADD COLUMN never_prune INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE memory ADD COLUMN never_decay INTEGER NOT NULL DEFAULT 0;",
            "ALTER TABLE memory ADD COLUMN needs_review INTEGER NOT NULL DEFAULT 0;",
        ];
        for sql in alters {
            let col = column_name_from_add(sql).unwrap_or("");
            if !has_column(tx, col)? {
                tx.execute_batch(sql).map_err(|e| {
                    MnemeError::Other(format!("migration v1->v2: {}", e))
                })?;
            }
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memory_active ON memory(deleted_at) WHERE deleted_at IS NULL;",
        )
        .map_err(|e| MnemeError::Other(format!("migration v1->v2 index: {}", e)))?;
        Ok(())
    }
}

/// v0.2 → v0.3: add agent-self-memory lifecycle fields
/// (status, due_at, claimed_by, parent_id, completed_at). All have
/// defaults so existing rows get status='active', others null. See
/// decisions.md D14.
pub struct V2ToV3;
impl Migration for V2ToV3 {
    fn target_version(&self) -> i64 {
        3
    }
    fn up(&self, tx: &Transaction) -> Result<()> {
        let alters: &[&str] = &[
            "ALTER TABLE memory ADD COLUMN status TEXT NOT NULL DEFAULT 'active';",
            "ALTER TABLE memory ADD COLUMN due_at INTEGER;",
            "ALTER TABLE memory ADD COLUMN claimed_by TEXT;",
            "ALTER TABLE memory ADD COLUMN parent_id TEXT;",
            "ALTER TABLE memory ADD COLUMN completed_at INTEGER;",
        ];
        for sql in alters {
            let col = column_name_from_add(sql).unwrap_or("");
            if !has_column(tx, col)? {
                tx.execute_batch(sql).map_err(|e| {
                    MnemeError::Other(format!("migration v2->v3: {}", e))
                })?;
            }
        }
        Ok(())
    }
}

/// All known migrations, in order. Append new entries here when
/// bumping `SCHEMA_VERSION`; no other code needs to change.
pub fn default_registry() -> Vec<Box<dyn Migration>> {
    vec![Box::new(V1ToV2), Box::new(V2ToV3)]
}

fn column_name_from_add(sql: &str) -> Option<&str> {
    sql.split("ADD COLUMN ").nth(1)?.split_whitespace().next()
}

fn has_column(tx: &Transaction, name: &str) -> Result<bool> {
    if name.is_empty() {
        return Ok(false);
    }
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('memory') WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SCHEMA_VERSION;

    /// Registry is sorted by version and ends at SCHEMA_VERSION.
    /// Catches the "forgot to bump SCHEMA_VERSION" footgun.
    #[test]
    fn registry_ends_at_schema_version() {
        let reg = default_registry();
        assert!(!reg.is_empty(), "registry must have at least one entry");
        let max = reg.iter().map(|m| m.target_version()).max().unwrap();
        assert_eq!(
            max, SCHEMA_VERSION as i64,
            "the highest migration's target_version must equal SCHEMA_VERSION"
        );
        // Strictly increasing.
        let mut last = 0;
        for m in &reg {
            assert!(m.target_version() > last, "versions must strictly increase");
            last = m.target_version();
        }
    }

    /// Each migration is idempotent: running `up()` twice on the
    /// same transaction state is a no-op (uses pragma_table_info
    /// guards). Half-migrated DBs from a crashed prior run must not
    /// fail when the next binary starts and re-runs the migration.
    #[test]
    fn migrations_are_idempotent() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        // SCHEMA_SQL is the canonical v3 shape — the store applies
        // it before running migrations, so we mirror that here.
        conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        // Fake schema_version = 1 so all migrations will run.
        conn.execute("DELETE FROM schema_version", []).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (1)",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        for m in default_registry() {
            m.up(&tx).unwrap();
            m.up(&tx).unwrap(); // second run: must not duplicate-column
            tx.execute(
                "UPDATE schema_version SET version = ?1",
                params![m.target_version()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    /// The order is the order migrations must apply — newer version
    /// first means the registry walks low → high. (Verified by the
    /// strictly-increasing check above; this test just makes the
    /// consequence explicit.)
    #[test]
    fn registry_runs_in_registered_order() {
        let reg = default_registry();
        for window in reg.windows(2) {
            assert!(window[0].target_version() < window[1].target_version());
        }
    }
}