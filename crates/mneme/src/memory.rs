//! High-level memory operations.
//!
//! `add` / `search` / `get` / `update` / `delete` / `list` over the
//! persistent store, with dedup, FTS5, auto-link, and confidence
//! updates wired in.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{MnemeError, Result};
use crate::forget;
use crate::schema::{Category, Memory, NewMemory, SearchHit, SearchOpts};
use crate::store::Store;

/// The full memory API. Wraps a [`Store`] and a [`Config`].
pub struct MemoryApi<'a> {
    pub store: &'a Store,
    pub config: &'a Config,
}

/// Result of `add`: includes the new memory id and any conflict
/// candidates surfaced from FTS5 similarity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddResult {
    pub id: String,
    pub conflicts: Vec<Memory>,
}

impl<'a> MemoryApi<'a> {
    pub fn new(store: &'a Store, config: &'a Config) -> Self {
        Self { store, config }
    }

    /// Compute SHA-256 of normalized content for dedup.
    pub fn content_hash(content: &str) -> String {
        let normalized: String = content
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| !c.is_whitespace())
            .collect();
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Quick secret/PII scan — returns the first offending pattern, or None.
    pub fn scan(content: &str) -> Option<&'static str> {
        // A small but high-signal list. Extend in `scanner.rs` later.
        const PATTERNS: &[(&str, &str)] = &[
            (r"AKIA[0-9A-Z]{16}", "AWS access key"),
            (r"sk-[A-Za-z0-9]{20,}", "OpenAI-style key"),
            (r"ghp_[A-Za-z0-9]{30,}", "GitHub PAT"),
            (r"xox[abp]-[A-Za-z0-9-]{10,}", "Slack token"),
            (r"AIza[0-9A-Za-z\-_]{35}", "Google API key"),
        ];
        for (pat, desc) in PATTERNS {
            if let Ok(re) = regex::Regex::new(pat) {
                if re.is_match(content) {
                    return Some(desc);
                }
            }
        }
        None
    }

    /// Topic key normalization: lowercase, hyphenated, capped at 64 chars.
    pub fn topic_key(title: &str, content: &str) -> String {
        let raw = format!("{} {}", title, content);
        let lower = raw.to_ascii_lowercase();
        let mut out = String::with_capacity(64);
        let mut prev_dash = false;
        for c in lower.chars() {
            if c.is_alphanumeric() {
                out.push(c);
                prev_dash = false;
            } else if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
            if out.len() >= 64 {
                break;
            }
        }
        let trimmed = out.trim_end_matches('-').to_string();
        if trimmed.is_empty() {
            "general".into()
        } else {
            trimmed
        }
    }

    /// Add a new memory. Returns the new id and any FTS5 conflict candidates.
    pub fn add(&self, m: NewMemory) -> Result<AddResult> {
        if let Some(threat) = Self::scan(&m.content) {
            return Err(MnemeError::ScanBlocked(threat.to_string()));
        }
        let hash = Self::content_hash(&m.content);

        // dedup
        if let Some(existing) = self.store.find_by_hash(&hash)? {
            return Ok(AddResult {
                id: existing.id,
                conflicts: vec![],
            });
        }

        let now = Utc::now();
        let topic = Self::topic_key(&m.title, &m.content);
        let memory = Memory {
            id: Uuid::now_v7().to_string(),
            memory_type: m.memory_type,
            tier: m.tier,
            category: m.category,
            title: m.title,
            content: m.content,
            context: m.context,
            topic_key: Some(topic),
            tags: m.tags,
            project: m.project,
            source: m.source,
            initial_confidence: self.config.forgetting.initial_confidence_default,
            confidence: self.config.forgetting.initial_confidence_default,
            importance: m.importance.clamp(0.0, 1.0),
            access_count: 0,
            last_accessed_at: now,
            created_at: now,
            override_half_life: m.override_half_life,
            never_prune: m.never_prune,
            never_decay: m.never_decay,
            content_hash: hash,
            deleted_at: None,
            needs_review: m.needs_review,
        };

        let tx = self.store.conn.unchecked_transaction()?;
        Store::insert_memory_tx(&tx, &memory)?;
        // conflict candidates (FTS5)
        let conflicts = self.find_conflict_candidates_tx(&tx, &memory.content, &memory.id, 5)?;
        // auto-link
        if self.config.edges.auto_link_enabled {
            self.auto_link_tx(&tx, &memory, &conflicts)?;
        }
        self.store
            .log_event("memory_add", Some(&memory.id), None, None, "agent")?;
        tx.commit()?;

        Ok(AddResult {
            id: memory.id,
            conflicts,
        })
    }

    fn find_conflict_candidates_tx(
        &self,
        tx: &rusqlite::Transaction,
        content: &str,
        exclude_id: &str,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        // use the first ~10 words as the FTS5 query, stripping anything
        // that FTS5 syntax doesn't accept.
        let fts_query = sanitize_fts_query(content, 10);
        if fts_query.is_empty() {
            return Ok(vec![]);
        }

        let mut stmt = tx.prepare(
            r#"SELECT m.* FROM memory m
               JOIN memory_fts fts ON fts.rowid = m.rowid
               WHERE memory_fts MATCH ?1
                 AND m.deleted_at IS NULL
                 AND m.id != ?2
               ORDER BY rank
               LIMIT ?3"#,
        )?;
        let rows = stmt.query_map(
            params![fts_query, exclude_id, limit as i64],
            Store::row_to_memory,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn auto_link_tx(
        &self,
        tx: &rusqlite::Transaction,
        new_mem: &Memory,
        conflicts: &[Memory],
    ) -> Result<()> {
        use crate::edge::EdgeApi;
        let edge_api = EdgeApi::new(self.store, self.config);

        // 1. topic_key match → related edge
        if let Some(tk) = &new_mem.topic_key {
            let same_topic: Vec<Memory> = {
                let mut stmt = tx.prepare(
                    r#"SELECT * FROM memory
                       WHERE topic_key = ?1 AND id != ?2 AND deleted_at IS NULL
                       LIMIT 10"#,
                )?;
                let rows = stmt.query_map(params![tk, &new_mem.id], Store::row_to_memory)?;
                rows.filter_map(|r| r.ok()).collect()
            };
            for other in same_topic {
                if other.id == new_mem.id {
                    continue;
                }
                let _ = edge_api.link_in_tx(
                    tx,
                    &new_mem.id,
                    &other.id,
                    crate::schema::EdgeType::Related,
                    self.config.edges.auto_link_topic_strength,
                    Some("auto:topic_match"),
                    Some(tk.as_str()),
                );
            }
        }

        // 2. supersede detection on conflicts
        if matches!(
            new_mem.category,
            Category::Decision | Category::Correction | Category::Preference
        ) {
            for old in conflicts {
                if old.created_at > new_mem.created_at {
                    continue;
                }
                let sim = jaccard(&new_mem.content, &old.content);
                let cfg = &self.config.edges;
                if (cfg.auto_link_supersede_min_sim..=cfg.auto_link_supersede_max_sim)
                    .contains(&sim)
                {
                    let _ = edge_api.link_in_tx(
                        tx,
                        &new_mem.id,
                        &old.id,
                        crate::schema::EdgeType::Supersedes,
                        0.9,
                        Some("auto:supersede_detection"),
                        Some(format!("jaccard={:.2}", sim).as_str()),
                    );
                }
            }
        }
        Ok(())
    }

    /// Get a memory by id.
    pub fn get(&self, id: &str) -> Result<Option<Memory>> {
        self.store.get_by_id(id)
    }

    /// Update a memory (full replacement).
    pub fn update(&self, m: &Memory) -> Result<()> {
        let tx = self.store.conn.unchecked_transaction()?;
        Store::update_memory_tx(&tx, m)?;
        self.store
            .log_event("memory_update", Some(&m.id), None, None, "agent")?;
        tx.commit()?;
        Ok(())
    }

    /// Soft-delete a memory (recovers within 30 days).
    pub fn soft_delete(&self, id: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        self.store.conn.execute(
            "UPDATE memory SET deleted_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        self.store
            .log_event("memory_soft_delete", Some(id), None, None, "agent")?;
        Ok(())
    }

    /// List active memories (no soft-deleted).
    pub fn list(&self, limit: usize) -> Result<Vec<Memory>> {
        self.store.list_active(limit)
    }

    /// Search using FTS5 with confidence + importance scoring.
    pub fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<SearchHit>> {
        let limit = opts.limit.unwrap_or(self.config.search.default_limit);
        let fts_query = sanitize_fts_query(query, 64);
        if fts_query.is_empty() {
            return Ok(vec![]);
        }
        let now = Utc::now();
        let mut sql = String::from(
            r#"SELECT m.*, rank FROM memory m
               JOIN memory_fts fts ON fts.rowid = m.rowid
               WHERE memory_fts MATCH ?1
                 AND m.deleted_at IS NULL"#,
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
        if let Some(c) = opts.category {
            sql.push_str(" AND m.category = ?");
            param_values.push(Box::new(c.as_str().to_string()));
        }
        if let Some(t) = opts.memory_type {
            sql.push_str(" AND m.memory_type = ?");
            param_values.push(Box::new(t.as_str().to_string()));
        }
        if let Some(p) = &opts.project {
            sql.push_str(" AND m.project = ?");
            param_values.push(Box::new(p.clone()));
        }
        sql.push_str(" ORDER BY rank LIMIT ?");
        param_values.push(Box::new((limit * 3) as i64)); // fetch more, sort & trim

        let mut stmt = self.store.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> = param_values.iter().map(|b| &**b).collect();
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            let m = Store::row_to_memory(row)?;
            // bm25 rank: lower is better; flip sign so higher = more relevant
            let rank: f64 = row.get("rank").unwrap_or(0.0);
            let bm25 = -rank as f32;
            Ok((m, bm25))
        })?;

        let mut hits: Vec<SearchHit> = Vec::new();
        for r in rows {
            let (m, bm25) = r?;
            let retrievability = forget::current_confidence(&m, self.config, now);
            if let Some(min) = opts.min_confidence {
                if retrievability < min {
                    continue;
                }
            }
            let importance_boost = 1.0 + m.importance;
            let score = bm25
                * self.config.search.weight_relevance.max(0.0)
                * retrievability.powf(self.config.search.weight_recency)
                * importance_boost.powf(self.config.search.weight_importance);
            hits.push(SearchHit {
                memory: m,
                score,
                bm25,
                retrievability,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);

        // update access stats
        let ids: Vec<String> = hits.iter().map(|h| h.memory.id.clone()).collect();
        for id in ids {
            if let Ok(Some(mut m)) = self.store.get_by_id(&id) {
                forget::on_access(&mut m, self.config, now);
                let _ = self.update(&m);
            }
        }
        Ok(hits)
    }
}

/// Sanitize user input into a safe FTS5 prefix query.
/// Strips non-alphanumeric chars (only _ is kept as a word char), drops
/// tokens < 3 chars, and joins remaining tokens with a single space,
/// each suffixed with `*`.
pub(crate) fn sanitize_fts_query(input: &str, max_tokens: usize) -> String {
    input
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty() && w.len() >= 3)
        .map(|w| format!("{}*", w))
        .take(max_tokens)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Crude Jaccard similarity over word sets.
fn jaccard(a: &str, b: &str) -> f32 {
    let wa: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let wb: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

// ── Store convenience methods (used by memory::MemoryApi) ─────────────

impl Store {
    pub(crate) fn find_by_hash(&self, hash: &str) -> Result<Option<Memory>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM memory WHERE content_hash = ?1 AND deleted_at IS NULL")?;
        let mut rows = stmt.query(params![hash])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Store::row_to_memory(row)?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn get_by_id(&self, id: &str) -> Result<Option<Memory>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM memory WHERE id = ?1 AND deleted_at IS NULL")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Store::row_to_memory(row)?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn list_active(&self, limit: usize) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM memory WHERE deleted_at IS NULL ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], Store::row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::NewMemory;

    fn setup() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }

    fn note(title: &str, content: &str) -> NewMemory {
        NewMemory::note(content, title)
    }

    #[test]
    fn add_and_get() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        let r = api.add(note("hello", "world")).unwrap();
        let got = api.get(&r.id).unwrap().unwrap();
        assert_eq!(got.title, "hello");
        assert_eq!(got.content, "world");
    }

    #[test]
    fn dedup_returns_existing() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        let r1 = api.add(note("x", "abc")).unwrap();
        let r2 = api.add(note("x", "abc")).unwrap();
        assert_eq!(r1.id, r2.id, "duplicate should return existing id");
    }

    #[test]
    fn search_finds_match() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        api.add(note("auth library", "use jose not jsonwebtoken"))
            .unwrap();
        let hits = api.search("jose", SearchOpts::default()).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].memory.content.contains("jose"));
    }

    #[test]
    fn scanner_blocks_secrets() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        let bad = note("x", "my key is AKIAIOSFODNN7EXAMPLE");
        let r = api.add(bad);
        assert!(matches!(r, Err(MnemeError::ScanBlocked(_))));
    }

    #[test]
    fn topic_key_normalization() {
        assert_eq!(
            MemoryApi::topic_key("Hello World", "foo bar"),
            "hello-world-foo-bar"
        );
    }
}
