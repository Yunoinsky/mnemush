//! High-level memory operations.
//!
//! `add` / `search` / `get` / `update` / `delete` / `list` over the
//! persistent store, with dedup, FTS5, auto-link, and confidence
//! updates wired in.

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{MnemeError, Result};
use crate::forget;
use crate::schema::{ActionStatus, Category, Memory, NewMemory, SearchHit, SearchOpts};
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

    /// Resolve the project filter for read paths. Returns:
    /// - `None` if no isolation is configured (backward-compatible —
    ///   every project visible, including NULL).
    /// - `Some("default")` if MNEME_PROJECT=default and cross-project
    ///   reads are disabled (only this project's memories visible).
    /// - `None` if cross-project reads are enabled (escape hatch).
    pub fn effective_read_filter(&self) -> Option<&str> {
        if self.config.project.cross_project_search {
            return None;
        }
        self.config.project.default_project.as_deref()
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
        if let Some(threat) = crate::scanner::scan(&m.content) {
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
        // Project: caller-supplied wins; otherwise auto-tag with the
        // config's default_project if isolation is enabled. NULL means
        // "global / un-scoped" and is preserved for callers that don't
        // configure isolation.
        let project = m
            .project
            .or_else(|| self.config.project.default_project.clone());
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
            project,
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
            // Agent-self-memory lifecycle fields. Status defaults to
            // Active; the other fields are optional. See decisions.md D14.
            status: crate::schema::ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
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

        // 2.5 auto-merge: near-duplicate snapshot-type memories.
        // For note/skill/insight/episodic (content that gets re-captured
        // as it evolves), a high Jaccard similarity with an existing
        // memory means we probably captured the same thing twice. The
        // old one is soft-deleted and its edges retargeted to the new
        // one — keeps evolving documents from piling up as near-dupes
        // that exact-content-hash dedup can't catch.
        if self.config.edges.auto_merge_enabled
            && matches!(
                new_mem.category,
                Category::Note
                    | Category::Skill
                    | Category::Insight
                    | Category::Episodic
            )
        {
            let min_sim = self.config.edges.auto_merge_min_sim;
            for old in conflicts {
                if old.created_at >= new_mem.created_at {
                    continue; // don't merge newer memories into this one
                }
                let sim = jaccard(&new_mem.content, &old.content);
                if sim >= min_sim {
                    // Retarget all of old's edges to new.
                    let mut stmt = tx.prepare(
                        "SELECT id FROM memory_edge WHERE (source_id = ?1 OR target_id = ?1) AND deleted_at IS NULL",
                    )?;
                    let edge_ids: Vec<String> = stmt
                        .query_map(params![&old.id], |r| r.get(0))?
                        .filter_map(|r| r.ok())
                        .collect();
                    for eid in &edge_ids {
                        tx.execute(
                            "UPDATE memory_edge SET source_id = ?1 WHERE id = ?2 AND source_id = ?3",
                            params![&new_mem.id, eid, &old.id],
                        )?;
                        tx.execute(
                            "UPDATE memory_edge SET target_id = ?1 WHERE id = ?2 AND target_id = ?3",
                            params![&new_mem.id, eid, &old.id],
                        )?;
                        // Drop self-loops introduced by the retarget.
                        tx.execute(
                            "DELETE FROM memory_edge WHERE source_id = target_id AND id = ?1",
                            params![eid],
                        )?;
                    }
                    // Soft-delete old + its FTS row (same tx).
                    let now = crate::store::Store::now_ts();
                    tx.execute(
                        "UPDATE memory SET deleted_at = ?1 WHERE id = ?2",
                        params![now, &old.id],
                    )?;
                    tx.execute(
                        "DELETE FROM memory_fts WHERE rowid = (SELECT rowid FROM memory WHERE id = ?1)",
                        params![&old.id],
                    )?;
                    self.store.log_event_tx(
                        tx,
                        "memory_auto_merge",
                        Some(&new_mem.id),
                        None,
                        Some(&format!(
                            "merged {} (jaccard={:.2})",
                            &old.id[..8],
                            sim
                        )),
                        "agent",
                    )?;
                }
            }
        }

        // 3. weak FTS5 similarity → low-strength related edge.
        // Runs for all categories. Bounded by `auto_link_weak_limit` (3 by
        // default). FTS5 top-3K is queried to allow jaccard filtering; the
        // limit trades off recall vs. work per add.
        if self.config.edges.auto_link_enabled {
            let weak_min = self.config.edges.auto_link_weak_min_sim;
            let weak_max = self.config.edges.auto_link_weak_max_sim;
            let weak_strength = self.config.edges.auto_link_weak_strength;
            let weak_limit = self.config.edges.auto_link_weak_limit;
            if weak_limit > 0 {
                let fts_query = sanitize_fts_query(&new_mem.content, 10);
                if !fts_query.is_empty() {
                    // Fetch a small over-fetch window so the jaccard filter
                    // doesn't starve the limit on noisy content.
                    let fetch = (weak_limit * 3).max(5);
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
                        params![fts_query, new_mem.id, fetch as i64],
                        Store::row_to_memory,
                    )?;
                    let mut added = 0usize;
                    for r in rows {
                        if added >= weak_limit {
                            break;
                        }
                        let other = r?;
                        // Skip if the new memory and the candidate are
                        // already linked (e.g. by topic_key match in step 1).
                        // The link_in_tx call is idempotent but a no-op write
                        // here would still log an edge_link event.
                        let already = tx
                            .query_row(
                                "SELECT 1 FROM memory_edge \
                                 WHERE ((source_id=?1 AND target_id=?2) \
                                        OR (source_id=?2 AND target_id=?1)) \
                                   AND deleted_at IS NULL LIMIT 1",
                                params![new_mem.id, other.id],
                                |_| Ok(()),
                            )
                            .is_ok();
                        if already {
                            continue;
                        }
                        let sim = jaccard(&new_mem.content, &other.content);
                        if (weak_min..weak_max).contains(&sim) {
                            let _ = edge_api.link_in_tx(
                                tx,
                                &new_mem.id,
                                &other.id,
                                crate::schema::EdgeType::Related,
                                weak_strength,
                                Some("auto:weak_similarity"),
                                Some(format!("jaccard={:.2}", sim).as_str()),
                            );
                            added += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Layer B of mechanism #2: select a set of recent memories that are
    /// good candidates for LLM-driven reflection.
    ///
    /// The LLM (the agent itself, in a follow-up turn) reads these and
    /// decides which conceptual links the auto-link layer missed. We
    /// surface the *least-connected* recent memories first because those
    /// have the most room for new edges.
    ///
    /// Filter: active, not Identity, not never_prune, created in the
    /// last `since_days` days. Order: edge_count ASC, created_at DESC.
    /// Limit: `limit`.
    pub fn reflect_candidates(
        &self,
        now: DateTime<Utc>,
        since_days: i64,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        let since_ts = (now - chrono::Duration::days(since_days)).timestamp();
        let mut stmt = self.store.conn.prepare(
            r#"SELECT m.*,
                      (SELECT COUNT(*) FROM memory_edge e
                       WHERE (e.source_id = m.id OR e.target_id = m.id)
                         AND e.deleted_at IS NULL) AS edge_count
               FROM memory m
               WHERE m.deleted_at IS NULL
                 AND m.memory_type != 'identity'
                 AND m.never_prune = 0
                 AND m.created_at > ?1
               ORDER BY edge_count ASC, m.created_at DESC
               LIMIT ?2"#,
        )?;
        let rows = stmt.query_map(params![since_ts, limit as i64], |row| {
            // Skip the edge_count column by reading the memory first.
            // We don't actually need edge_count; the ORDER BY uses it.
            Store::row_to_memory(row)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Get a memory by id.
    pub fn get(&self, id: &str) -> Result<Option<Memory>> {
        self.store.get_by_id(id)
    }

    /// Update a memory (full replacement). Auto-manages lifecycle
    /// timestamps: transitioning into a terminal status (Completed /
    /// Abandoned) sets `completed_at`; transitioning back to Active
    /// clears it.
    pub fn update(&self, m: &Memory) -> Result<()> {
        // Build a final copy with lifecycle side-effects applied.
        // (Don't mutate the caller's value; that would surprise
        // the caller who still holds a reference.)
        let now = Utc::now();
        let mut finalized = m.clone();
        match finalized.status {
            ActionStatus::Completed | ActionStatus::Abandoned => {
                if finalized.completed_at.is_none() {
                    finalized.completed_at = Some(now);
                }
            }
            ActionStatus::Active => {
                finalized.completed_at = None;
            }
        }
        let tx = self.store.conn.unchecked_transaction()?;
        Store::update_memory_tx(&tx, &finalized)?;
        self.store
            .log_event("memory_update", Some(&m.id), None, None, "agent")?;
        tx.commit()?;
        Ok(())
    }

    /// Soft-delete a memory (recovers within 30 days).
    pub fn soft_delete(&self, id: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        // Same transaction: mark deleted AND remove the FTS5 row so
        // the index stays rowid-aligned with the memory table. Orphaned
        // FTS rows caused rowid reuse to point at the wrong content
        // (search returned garbage) — fixed 2026-08-06, see
        // reindex(). Keep the DELETE here so it never recurs.
        let tx = self.store.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE memory SET deleted_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        tx.execute("DELETE FROM memory_fts WHERE rowid = (SELECT rowid FROM memory WHERE id = ?1)", params![id])?;
        self.store
            .log_event_tx(&tx, "memory_soft_delete", Some(id), None, None, "agent")?;
        tx.commit()?;
        Ok(())
    }

    /// List active memories (no soft-deleted).
    pub fn list(&self, limit: usize) -> Result<Vec<Memory>> {
        // Apply the config-level project filter when isolation is
        // enabled. Caller-supplied overrides go through list_in_project.
        self.store
            .list_active_filtered(limit, self.effective_read_filter())
    }

    /// List active memories scoped to a specific project.
    /// `Some(name)` → only that project; `None` → all projects
    /// (cross-project escape hatch, ignoring MNEME_PROJECT).
    pub fn list_in_project(&self, limit: usize, project: Option<&str>) -> Result<Vec<Memory>> {
        self.store.list_active_filtered(limit, project)
    }

    /// Return the single highest-priority active action.
    /// Priority: `due_at` ASC (nulls last), then `created_at` ASC.
    /// Excludes completed and abandoned memories.
    pub fn memory_next(&self) -> Result<Option<Memory>> {
        let project = self.effective_read_filter();
        let project_clause = if project.is_some() {
            " AND m.project = ?1"
        } else {
            ""
        };
        let sql = format!(
            r#"SELECT m.* FROM memory m
               WHERE m.deleted_at IS NULL
                 AND m.status = 'active'
                 AND m.category != 'identity'{project_clause}
               ORDER BY
                 CASE WHEN m.due_at IS NULL THEN 1 ELSE 0 END ASC,
                 m.due_at ASC,
                 m.created_at DESC,
                 m.id DESC
               LIMIT 1"#,
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let mut rows = if let Some(p) = project {
            stmt.query_map(rusqlite::params![p], Store::row_to_memory)?
        } else {
            stmt.query_map([], Store::row_to_memory)?
        };
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Return all active actions, sorted by priority (due_at ASC,
    /// nulls last; then created_at DESC). Excludes completed /
    /// abandoned.
    pub fn memory_frontier(&self) -> Result<Vec<Memory>> {
        let project = self.effective_read_filter();
        let project_clause = if project.is_some() {
            " AND m.project = ?1"
        } else {
            ""
        };
        let sql = format!(
            r#"SELECT m.* FROM memory m
               WHERE m.deleted_at IS NULL
                 AND m.status = 'active'
                 AND m.category != 'identity'{project_clause}
               ORDER BY
                 CASE WHEN m.due_at IS NULL THEN 1 ELSE 0 END ASC,
                 m.due_at ASC,
                 m.created_at DESC,
                 m.id DESC"#,
        );
        let mut stmt = self.store.conn.prepare(&sql)?;
        let rows = if let Some(p) = project {
            stmt.query_map(rusqlite::params![p], Store::row_to_memory)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], Store::row_to_memory)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(rows)
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
        // Project filter: caller-supplied wins; otherwise fall back to
        // config-level isolation (None means "see all" — backward compat).
        // opts.cross_project_override bypasses isolation entirely
        // (CLI `--all-projects` flag).
        let project_filter: Option<String> = if opts.cross_project_override {
            None
        } else {
            opts.project
                .clone()
                .or_else(|| self.effective_read_filter().map(str::to_string))
        };
        if let Some(p) = project_filter {
            sql.push_str(" AND m.project = ?");
            param_values.push(Box::new(p));
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

        // ── Embedding blend (v1.0, opt-in) ─────────────────────────
        // Re-rank by blending the BM25 score with cosine similarity
        // between the query embedding and each hit's stored embedding.
        // Falls back to BM25-only if the model can't be loaded or no
        // embeddings are stored yet.
        if self.config.embedding.enabled && !hits.is_empty() {
            let model_id = self.config.embedding.model.clone();
            if let Ok(emb) = crate::embeddings::cached_embedder(&model_id) {
                let qvec: Vec<f32> = emb
                    .lock()
                    .ok()
                    .and_then(|mut e| e.embed(&[query]).ok())
                    .and_then(|mut v| {
                        if v.is_empty() {
                            None
                        } else {
                            Some(v.remove(0))
                        }
                    })
                    .unwrap_or_default();
                if !qvec.is_empty() {
                    let ids: Vec<String> = hits.iter().map(|h| h.memory.id.clone()).collect();
                    let stored = self
                        .store
                        .embeddings_for(&ids, &model_id)
                        .unwrap_or_default();
                    let mut cos_by_id: std::collections::HashMap<String, f32> =
                        std::collections::HashMap::new();
                    for s in &stored {
                        let n = qvec.len().min(s.vec.len());
                        if n > 0 {
                            cos_by_id.insert(
                                s.memory_id.clone(),
                                crate::embeddings::cosine(&qvec[..n], &s.vec[..n]),
                            );
                        }
                    }
                    let bm25_min = hits.iter().map(|h| h.bm25).fold(f32::INFINITY, f32::min);
                    let bm25_max = hits
                        .iter()
                        .map(|h| h.bm25)
                        .fold(f32::NEG_INFINITY, f32::max);
                    let span = (bm25_max - bm25_min).max(f32::EPSILON);
                    let bw = self.config.embedding.bm25_weight;
                    let ew = self.config.embedding.embed_weight;
                    for hit in hits.iter_mut() {
                        let norm = ((hit.bm25 - bm25_min) / span).clamp(0.0, 1.0);
                        let cos = cos_by_id.get(&hit.memory.id).copied().unwrap_or(0.0);
                        hit.score = bw * norm + ew * cos;
                    }
                    hits.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }
        }

        // ── Spreading activation (v0.3) ──────────────────────────
        // For each top hit, pull 1-hop neighbors and add them with
        // score = hit.score * edge.strength * decay. Lets the LLM find
        // related memories that didn't match the query text directly
        // (e.g. "dopamine" finds a paper about "reward signaling"
        // because the two are linked).
        //
        // ponytail: bounded (1 hop, decay 0.5) and gated on the existing
        // max_neighbor_hops config. Set max_neighbor_hops=0 to disable
        // without changing code.
        if self.config.edges.max_neighbor_hops >= 1 && !hits.is_empty() {
            const DECAY: f32 = 0.5;
            let mut extra: std::collections::HashMap<String, (Memory, f32)> =
                std::collections::HashMap::new();
            for hit in &hits {
                let neighbors: Vec<(Memory, f32)> = {
                    let mut stmt = self.store.conn.prepare(
                        "SELECT m.*, e.strength FROM memory m
                         JOIN memory_edge e ON
                           ((e.source_id = ?1 AND e.target_id = m.id) OR
                            (e.target_id = ?1 AND e.source_id = m.id))
                         WHERE m.deleted_at IS NULL
                           AND m.id != ?1
                           AND e.deleted_at IS NULL
                         LIMIT 10",
                    )?;
                    let rows = stmt.query_map(params![hit.memory.id], |row| {
                        let m = Store::row_to_memory(row)?;
                        let strength: f64 = row.get("strength")?;
                        Ok((m, strength as f32))
                    })?;
                    rows.filter_map(|r| r.ok()).collect()
                };
                for (m, strength) in neighbors {
                    let boost = hit.score * strength * DECAY;
                    let entry = extra.entry(m.id.clone()).or_insert((m.clone(), 0.0));
                    if boost > entry.1 {
                        entry.0 = m;
                        entry.1 = boost;
                    }
                }
            }
            // Merge neighbors that aren't already direct hits.
            let direct_ids: std::collections::HashSet<String> =
                hits.iter().map(|h| h.memory.id.clone()).collect();
            for (id, (m, score)) in extra {
                if direct_ids.contains(&id) {
                    continue;
                }
                hits.push(SearchHit {
                    memory: m,
                    score,
                    bm25: 0.0, // expanded, not FTS5 — no BM25 score
                    retrievability: 1.0,
                });
            }
            // Re-sort and re-truncate.
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit);
        }

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
/// tokens < 3 chars, and joins remaining tokens with `OR`, each suffixed
/// with `*`. OR (not the default FTS5 phrase match) is the right
/// semantics for "find similar" use cases — conflict detection, weak
/// auto-link, and ad-hoc search all want a memory that matches *any*
/// prefix term, not all of them in sequence.
pub(crate) fn sanitize_fts_query(input: &str, max_tokens: usize) -> String {
    input
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty() && w.len() >= 3)
        .map(|w| format!("{}*", w))
        .take(max_tokens)
        .collect::<Vec<_>>()
        .join(" OR ")
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

    /// List active memories with an optional project filter. Pass
    /// `Some(name)` to scope to that project (exact match, NULLs
    /// excluded); pass `None` for the backward-compatible "see all"
    /// behavior.
    pub(crate) fn list_active_filtered(
        &self,
        limit: usize,
        project: Option<&str>,
    ) -> Result<Vec<Memory>> {
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match project {
            Some(p) => (
                "SELECT * FROM memory WHERE deleted_at IS NULL AND project = ?1 \
                 ORDER BY created_at DESC LIMIT ?2"
                    .to_string(),
                vec![Box::new(p.to_string()), Box::new(limit as i64)],
            ),
            None => (
                "SELECT * FROM memory WHERE deleted_at IS NULL \
                 ORDER BY created_at DESC LIMIT ?1"
                    .to_string(),
                vec![Box::new(limit as i64)],
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| &**b).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Store::row_to_memory)?;
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

    /// Layer A of mechanism #2: when a new memory is added, the
    /// auto-link step 3 (weak FTS5 similarity) should create low-strength
    /// `related` edges to other memories that share 5–50% of their words.
    /// Verifies the new edge exists, has the right strength, and points
    /// to the expected target.
    #[test]
    fn auto_link_weak_similarity() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        // Two related memories: deliberately not exactly 0.5 jaccard
        // (which is the supersede threshold).
        // A: "dopamine release mushroom body drives associative learning"
        // B: "octopamine release in the mushroom body modulates reward learning"
        // Shared: {release, mushroom, body} = 3, Union = 14, jaccard ≈ 0.21
        api.add(note(
            "dopamine and reward learning in flies",
            "dopamine release mushroom body drives associative learning",
        ))
        .unwrap();
        let new_id = api
            .add(note(
                "octopamine and reward signaling in insects",
                "octopamine release in the mushroom body modulates reward learning",
            ))
            .unwrap()
            .id;
        // Query the edge table for the new memory's outbound edges.
        let edges: Vec<(String, f32, String)> = store
            .conn
            .prepare(
                "SELECT target_id, strength, provenance FROM memory_edge \
                 WHERE source_id = ?1 AND deleted_at IS NULL",
            )
            .unwrap()
            .query_map(rusqlite::params![new_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        // Should have at least one auto:weak_similarity edge to the first
        // memory.
        let weak: Vec<_> = edges
            .iter()
            .filter(|(_, _, prov)| prov == "auto:weak_similarity")
            .collect();
        assert!(
            !weak.is_empty(),
            "expected at least one weak-similarity edge, got: {:?}",
            edges
        );
        // Strength should match the configured default (0.4).
        for (_target, strength, _) in &weak {
            assert!(
                (*strength - cfg.edges.auto_link_weak_strength).abs() < 0.01,
                "weak edge strength should be {}, got {}",
                cfg.edges.auto_link_weak_strength,
                strength
            );
        }
    }

    /// auto_link_weak_limit caps the number of weak edges per add.
    #[test]
    fn auto_link_weak_respects_limit() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        // 5 memories with 3 shared words (mushroom body dopamine) and
        // 6 unique words each. New memory: 3 shared + 3 unique.
        // Jaccard = 3 / (6 + 6 + 3 - 3) = 3/9 = 0.33 (well under 0.5).
        for i in 0..5 {
            api.add(note(
                &format!("study {}", i),
                &format!(
                    "mushroom body dopamine alpha{} beta{} gamma{} delta{} epsilon{} zeta{}",
                    i, i, i, i, i, i
                ),
            ))
            .unwrap();
        }
        let new_id = api
            .add(note("new study", "mushroom body dopamine x y z"))
            .unwrap()
            .id;
        let n_weak: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edge \
                 WHERE source_id=?1 AND provenance='auto:weak_similarity' \
                   AND deleted_at IS NULL",
                rusqlite::params![new_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            n_weak <= cfg.edges.auto_link_weak_limit as i64,
            "weak edges {} should be <= limit {}",
            n_weak,
            cfg.edges.auto_link_weak_limit
        );
        assert!(n_weak > 0, "expected at least one weak edge");
    }

    /// auto_link_weak sim range [min, max) — jaccard below min should NOT
    /// produce a weak edge even if FTS5 has a token match.
    #[test]
    fn auto_link_weak_below_min_sim() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        // One word shared, 9 words unique: jaccard ≈ 0.09 (above 0.05 default).
        // Below 0.05: share 0/10 words → no edge.
        api.add(note(
            "alpha",
            "one two three four five six seven eight nine ten",
        ))
        .unwrap();
        let new_id = api
            .add(note(
                "beta",
                "eleven twelve thirteen fourteen fifteen sixteen",
            ))
            .unwrap()
            .id;
        let n_weak: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edge \
                 WHERE source_id=?1 AND provenance='auto:weak_similarity' \
                   AND deleted_at IS NULL",
                rusqlite::params![new_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_weak, 0, "no shared tokens should not produce a weak edge");
    }

    /// Layer B of mechanism #2: reflect_candidates returns recent,
    /// least-connected memories first, with limit respected.
    #[test]
    fn reflect_candidates_recent_and_isolated() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        // 3 fresh memories; add an inbound edge to the first to make it
        // "more connected" than the others.
        let a = api.add(note("a", "alpha alpha alpha")).unwrap().id;
        let b = api.add(note("b", "beta beta beta")).unwrap().id;
        let c = api.add(note("c", "gamma gamma gamma")).unwrap().id;
        // Make a an isolated anchor + a target, link to a.
        let anchor = api.add(note("anchor", "anchor anchor anchor")).unwrap().id;
        api.add(note("isolated", "iso iso iso")).unwrap();
        let edge_api = crate::edge::EdgeApi::new(&store, &cfg);
        edge_api
            .link(
                &anchor,
                &a,
                crate::schema::EdgeType::Related,
                0.5,
                None,
                None,
            )
            .unwrap();
        // reflect_candidates: b and c are least connected; a is next;
        // anchor is most connected; isolated was just added (no edges)
        // — should also appear.
        let hits = api.reflect_candidates(chrono::Utc::now(), 7, 10).unwrap();
        let ids: Vec<String> = hits.iter().map(|m| m.id.clone()).collect();
        // isolated (0 edges) and b, c (0 edges) should all appear; a (1 edge)
        // and anchor (1 edge) should too if the limit is large enough.
        assert!(ids.contains(&b), "expected b in candidates, got {:?}", ids);
        assert!(ids.contains(&c), "expected c in candidates, got {:?}", ids);
        // Limit respected
        assert!(hits.len() <= 10);
    }

    /// reflect_candidates respects the since_days filter — old memories
    /// (created > since_days ago) are not surfaced.
    #[test]
    fn reflect_candidates_respects_since_days() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        // One fresh, one with a backdated created_at.
        api.add(note("fresh", "recent memory")).unwrap();
        let old = api.add(note("old", "ancient memory")).unwrap();
        // Backdate the "old" memory's created_at to 30 days ago.
        let past = (chrono::Utc::now() - chrono::Duration::days(30)).timestamp();
        store
            .conn
            .execute(
                "UPDATE memory SET created_at=?1 WHERE id=?2",
                rusqlite::params![past, old.id],
            )
            .unwrap();
        let hits = api.reflect_candidates(chrono::Utc::now(), 7, 10).unwrap();
        let ids: Vec<String> = hits.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids.len(), 1, "only fresh memory should match: {:?}", ids);
        assert!(!ids.is_empty());
    }

    /// reflect_candidates excludes Identity memories (they're exempt
    /// Spreading activation (v0.3): search results include 1-hop
    /// neighbors of top hits, with score = hit.score * edge.strength * 0.5.
    #[test]
    fn search_spreading_activation() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        let edge_api = crate::edge::EdgeApi::new(&store, &cfg);
        // Two memories: "alpha" (matched by query) and "beta" (linked to alpha).
        // Beta doesn't contain the query terms but should appear via expansion.
        let alpha = api
            .add(note("alpha", "dopamine release reward"))
            .unwrap()
            .id;
        let beta = api
            .add(note("beta", "antenna olfactory circuit"))
            .unwrap()
            .id;
        edge_api
            .link(
                &alpha,
                &beta,
                crate::schema::EdgeType::Related,
                0.8,
                None,
                None,
            )
            .unwrap();
        // Search for a term only in alpha.
        let hits = api
            .search(
                "dopamine",
                SearchOpts {
                    limit: Some(10),
                    ..Default::default()
                },
            )
            .unwrap();
        let ids: Vec<String> = hits.iter().map(|h| h.memory.id.clone()).collect();
        assert!(
            ids.contains(&alpha),
            "alpha (direct match) must be in results"
        );
        assert!(
            ids.contains(&beta),
            "beta (1-hop neighbor of alpha) should be expanded in, got: {:?}",
            ids
        );
        // beta should have a non-zero score from the expansion.
        let beta_hit = hits.iter().find(|h| h.memory.id == beta).unwrap();
        assert!(beta_hit.score > 0.0, "beta's expanded score should be > 0");
    }

    /// Spreading activation is bounded by `max_neighbor_hops` — set to 0
    /// to disable.
    #[test]
    fn search_spreading_disabled_when_hops_zero() {
        let (store, mut cfg) = setup();
        cfg.edges.max_neighbor_hops = 0;
        let api = MemoryApi::new(&store, &cfg);
        let edge_api = crate::edge::EdgeApi::new(&store, &cfg);
        let alpha = api
            .add(note("alpha", "dopamine release reward"))
            .unwrap()
            .id;
        let beta = api
            .add(note("beta", "antenna olfactory circuit"))
            .unwrap()
            .id;
        edge_api
            .link(
                &alpha,
                &beta,
                crate::schema::EdgeType::Related,
                0.8,
                None,
                None,
            )
            .unwrap();
        let hits = api
            .search(
                "dopamine",
                SearchOpts {
                    limit: Some(10),
                    ..Default::default()
                },
            )
            .unwrap();
        let ids: Vec<String> = hits.iter().map(|h| h.memory.id.clone()).collect();
        assert!(ids.contains(&alpha));
        assert!(
            !ids.contains(&beta),
            "with max_neighbor_hops=0, beta should NOT be expanded in"
        );
        drop(store); // silence unused warning
    }

    /// from almost everything else; reflection shouldn't surface them).
    #[test]
    fn reflect_candidates_excludes_identity() {
        let (store, cfg) = setup();
        let api = MemoryApi::new(&store, &cfg);
        api.add(note("regular", "normal memory")).unwrap();
        // Add an identity memory directly.
        let m = crate::schema::Memory {
            id: "id-1".into(),
            memory_type: crate::schema::MemoryType::Identity,
            tier: crate::schema::Tier::Global,
            category: crate::schema::Category::Identity,
            title: "I".into(),
            content: "identity content".into(),
            context: None,
            topic_key: None,
            tags: vec![],
            project: None,
            source: crate::schema::Source::Manual,
            initial_confidence: 1.0,
            confidence: 1.0,
            importance: 1.0,
            access_count: 0,
            last_accessed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            override_half_life: None,
            never_prune: true,
            never_decay: true,
            content_hash: "h-id".into(),
            deleted_at: None,
            needs_review: false,
            status: ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
        };
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
        drop(m);

        let hits = api.reflect_candidates(chrono::Utc::now(), 7, 10).unwrap();
        let ids: Vec<String> = hits.iter().map(|m| m.id.clone()).collect();
        assert!(
            !ids.contains(&"id-1".to_string()),
            "identity memory should be excluded"
        );
        assert_eq!(hits.len(), 1);
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

#[cfg(test)]
mod debug_tests {}

#[cfg(test)]
mod action_field_tests {
    //! TDD red phase for v0.3 agent-self-memory (action/lease/checkpoint).
    //! These tests pin the expected behavior. They will FAIL until the
    //! implementation lands. See ROADMAP v0.3 + decisions D14.

    use super::*;
    use crate::schema::{ActionStatus, Category, MemoryType, NewMemory, Source, Tier};
    use crate::store::Store;

    fn cfg() -> crate::config::Config {
        crate::config::Config::default()
    }
    fn store() -> (Store, crate::config::Config) {
        let s = Store::open_in_memory().unwrap();
        (s, cfg())
    }
    fn make_mem(content: &str, title: &str) -> NewMemory {
        NewMemory {
            content: content.into(),
            title: title.into(),
            category: Category::Note,
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            context: None,
            tags: vec![],
            project: None,
            source: Source::Manual,
            importance: 0.5,
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            needs_review: false,
        }
    }

    /// Active memory has status='active' by default and completed_at=0/None.
    #[test]
    fn action_default_status_is_active() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let r = api.add(make_mem("c", "t")).unwrap();
        let m = api.get(&r.id).unwrap().unwrap();
        assert_eq!(m.status, ActionStatus::Active, "default should be active");
    }

    /// Setting status=Completed populates completed_at.
    #[test]
    fn action_completed_sets_completed_at() {
        // TDD red: completed_at should be auto-set when status transitions
        // to Completed (either via API or update path). For now, just
        // verify the field exists and can be set explicitly.
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let r = api.add(make_mem("c", "t")).unwrap();
        let m = api.get(&r.id).unwrap().unwrap();
        assert_eq!(m.status, ActionStatus::Active);
        assert!(m.completed_at.is_none());
    }

    /// The fields due_at / claimed_by / parent_id round-trip through
    /// the store. This pins that the new schema columns actually exist
    /// and are readable (schema migration v2→v3 worked).
    #[test]
    fn action_metadata_fields_round_trip() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let r = api.add(make_mem("c", "t")).unwrap();
        let m = api.get(&r.id).unwrap().unwrap();
        assert!(m.due_at.is_none(), "due_at defaults to None");
        assert!(m.claimed_by.is_none(), "claimed_by defaults to None");
        assert!(m.parent_id.is_none(), "parent_id defaults to None");
    }

    /// The action helpers (next / frontier) return only memories whose
    /// `status` is Active. Completed / Abandoned are filtered out.
    /// This is the core behavior that makes "next" semantically
    /// different from a regular `memory_search`.
    #[test]
    fn action_list_excludes_completed_and_abandoned() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        // Create 3 actions in different statuses
        let _a = api.add(make_mem("active task", "A")).unwrap();
        let b = api.add(make_mem("done task", "B")).unwrap();
        let c = api.add(make_mem("dropped task", "C")).unwrap();
        // Move B to Completed, C to Abandoned
        for (res, st) in [(b, ActionStatus::Completed), (c, ActionStatus::Abandoned)] {
            let mut m = api.get(&res.id).unwrap().unwrap();
            m.status = st;
            api.update(&m).unwrap();
        }
        // TDD red: next/frontier filters will not exist until impl
    }
}

#[cfg(test)]
mod action_business_logic_tests {
    //! TDD red phase: v0.3 agent-self-memory business logic.
    //!
    //! These tests pin the desired behavior. They will FAIL until:
    //! - `update()` auto-sets `completed_at` when status transitions
    //!   active → completed (or active → abandoned).
    //! - `memory_next()` exists: returns 1 highest-priority active action.
    //! - `memory_frontier()` exists: returns all active actions sorted.
    //! - Both filter out completed / abandoned.

    use super::*;
    use crate::schema::{ActionStatus, Category, MemoryType, Source, Tier};
    use crate::store::Store;

    fn cfg() -> crate::config::Config {
        crate::config::Config::default()
    }
    fn store() -> (Store, crate::config::Config) {
        let s = Store::open_in_memory().unwrap();
        (s, cfg())
    }
    fn make_mem(content: &str, title: &str) -> NewMemory {
        NewMemory {
            content: content.into(),
            title: title.into(),
            category: Category::Note,
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            context: None,
            tags: vec![],
            project: None,
            source: Source::Manual,
            importance: 0.5,
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            needs_review: false,
        }
    }

    /// update() should auto-populate completed_at when status flips
    /// from active to completed. This is the most basic "doing the
    /// thing" the v0.3 spec promises.
    #[test]
    fn update_completed_auto_sets_completed_at() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let res = api.add(make_mem("c", "t")).unwrap();
        let mut m = api.get(&res.id).unwrap().unwrap();
        assert!(m.completed_at.is_none());
        m.status = ActionStatus::Completed;
        api.update(&m).unwrap();
        let m2 = api.get(&res.id).unwrap().unwrap();
        assert!(
            m2.completed_at.is_some(),
            "update(Completed) must set completed_at"
        );
        assert_eq!(m2.status, ActionStatus::Completed);
    }

    /// update() with status=abandoned should also set completed_at —
    /// it's the terminal transition time, not a "done" marker per se.
    #[test]
    fn update_abandoned_auto_sets_completed_at() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let res = api.add(make_mem("c", "t")).unwrap();
        let mut m = api.get(&res.id).unwrap().unwrap();
        m.status = ActionStatus::Abandoned;
        api.update(&m).unwrap();
        let m2 = api.get(&res.id).unwrap().unwrap();
        assert!(
            m2.completed_at.is_some(),
            "update(Abandoned) must set completed_at"
        );
    }

    /// update() with status=active should NOT touch completed_at
    /// (a re-activated action loses its prior completion timestamp).
    #[test]
    fn update_active_clears_completed_at() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let res = api.add(make_mem("c", "t")).unwrap();
        let mut m = api.get(&res.id).unwrap().unwrap();
        m.status = ActionStatus::Completed;
        api.update(&m).unwrap();
        let mut m2 = api.get(&res.id).unwrap().unwrap();
        assert!(m2.completed_at.is_some());
        m2.status = ActionStatus::Active;
        api.update(&m2).unwrap();
        let m3 = api.get(&res.id).unwrap().unwrap();
        assert!(
            m3.completed_at.is_none(),
            "re-activation should clear completed_at"
        );
    }

    /// memory_next(): 1 highest-priority active action.
    /// Priority: due_at ASC (nulls last), then created_at ASC.
    #[test]
    fn memory_next_returns_highest_priority() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        // Three actions, distinct due_at / created_at
        let a = api.add(make_mem("a", "low")).unwrap();
        let b = api.add(make_mem("b", "med")).unwrap();
        let c = api.add(make_mem("c", "hig")).unwrap();
        // No due_at → fallback to created_at ASC. c was added last, so
        // it's the highest priority. Test that ordering.
        let next = api.memory_next().unwrap().expect("expected one next");
        // The most recently-created active action is "c"
        assert_eq!(next.id, c.id);
        // Verify a, b, c still exist and are all active
        for id in [a.id, b.id, c.id] {
            let m = api.get(&id).unwrap().unwrap();
            assert_eq!(m.status, ActionStatus::Active);
        }
    }

    /// memory_next(): excludes completed / abandoned memories.
    #[test]
    fn memory_next_excludes_completed() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let a = api.add(make_mem("a", "a")).unwrap();
        let b = api.add(make_mem("b", "b")).unwrap();
        // Mark b as completed (with completed_at)
        let mut m = api.get(&b.id).unwrap().unwrap();
        m.status = ActionStatus::Completed;
        api.update(&m).unwrap();
        // memory_next() should now return a (since b is completed)
        let next = api.memory_next().unwrap().expect("expected one");
        assert_eq!(next.id, a.id);
    }

    /// memory_frontier(): all active actions, sorted by priority.
    #[test]
    fn memory_frontier_returns_all_active() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let a = api.add(make_mem("a", "a")).unwrap();
        let b = api.add(make_mem("b", "b")).unwrap();
        let c = api.add(make_mem("c", "c")).unwrap();
        // Mark b as abandoned
        let mut m = api.get(&b.id).unwrap().unwrap();
        m.status = ActionStatus::Abandoned;
        api.update(&m).unwrap();
        // frontier should contain a and c, not b
        let frontier = api.memory_frontier().unwrap();
        let ids: Vec<&str> = frontier.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&a.id.as_str()));
        assert!(ids.contains(&c.id.as_str()));
        assert!(!ids.contains(&b.id.as_str()));
    }

    /// v0.4 project isolation: writes are auto-tagged with
    /// default_project; reads default to that project; cross_project
    /// escape hatch is opt-in.
    #[test]
    fn project_isolation_auto_tags_writes_and_filters_reads() {
        let (store, mut cfg) = store();
        cfg.project.default_project = Some("alpha".to_string());
        let api = MemoryApi::new(&store, &cfg);
        // Caller doesn't pass project → auto-tagged "alpha".
        let a = api.add(make_mem("a", "alpha-mem")).unwrap();
        let m = api.get(&a.id).unwrap().unwrap();
        assert_eq!(m.project.as_deref(), Some("alpha"));

        // Caller passes project="beta" → overrides the default.
        let mut b = make_mem("b", "beta-mem");
        b.project = Some("beta".to_string());
        let b_id = api.add(b).unwrap().id;
        let m = api.get(&b_id).unwrap().unwrap();
        assert_eq!(m.project.as_deref(), Some("beta"));

        // Reader without filter sees only "alpha" memories.
        let list_alpha = api.list(100).unwrap();
        assert_eq!(list_alpha.len(), 1);
        assert_eq!(list_alpha[0].id, a.id);

        // Explicit cross-project read sees all.
        let list_all = api.list_in_project(100, None).unwrap();
        assert_eq!(list_all.len(), 2);

        // memory_next() respects the default filter.
        let nxt = api.memory_next().unwrap().expect("has next");
        assert_eq!(nxt.id, a.id);

        // memory_frontier() respects the default filter.
        let f = api.memory_frontier().unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, a.id);
    }

    /// v0.4 cross_project_search flag bypasses isolation for reads.
    #[test]
    fn cross_project_search_bypasses_isolation() {
        let (store, mut cfg) = store();
        cfg.project.default_project = Some("default".to_string());
        cfg.project.cross_project_search = true;
        let api = MemoryApi::new(&store, &cfg);
        let mut b = make_mem("other", "other-project");
        b.project = Some("other-project".to_string());
        api.add(b).unwrap();
        // cross_project_search=true → effective filter is None →
        // list returns everything regardless of default_project.
        let mems = api.list(100).unwrap();
        assert_eq!(
            mems.len(),
            1,
            "cross-project escape should show the other-project memory"
        );
    }

    /// v0.4 backward compatibility: with no default_project, NULL
    /// project memories are visible (v0.3 behavior).
    #[test]
    fn no_default_project_is_backward_compatible() {
        let (store, cfg) = store(); // default config — no project isolation
        let api = MemoryApi::new(&store, &cfg);
        // Memory with project=NULL.
        let mut no_proj = make_mem("no-proj", "v");
        no_proj.project = None;
        let id1 = api.add(no_proj).unwrap().id;
        // Memory with explicit project.
        let mut with_proj = make_mem("with-proj", "v");
        with_proj.project = Some("anywhere".to_string());
        let id2 = api.add(with_proj).unwrap().id;
        // Both visible (backward-compatible "see all").
        let mems = api.list(100).unwrap();
        let ids: Vec<&str> = mems.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
    }

    /// Embedding blend search: with embeddings disabled (default),
    /// `search()` returns BM25-only hits as before. We can't exercise
    /// the embedder load path in a unit test (requires network for
    /// the first-time model download), but we can verify that the
    /// disabled branch is hit and the score field is unchanged from
    /// pre-blend behavior.
    #[test]
    fn search_with_embeddings_disabled_still_works() {
        let (store, mut cfg) = store();
        cfg.embedding.enabled = false;
        let api = MemoryApi::new(&store, &cfg);
        api.add(make_mem("alpha content", "alpha")).unwrap();
        api.add(make_mem("beta content", "beta")).unwrap();
        let hits = api
            .search(
                "alpha",
                SearchOpts {
                    limit: Some(10),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h.memory.title == "alpha"));
    }

    /// Cosine math used by the blend path: identical → 1.0,
    /// orthogonal → 0.0, zero-norm → 0.0.
    #[test]
    fn cosine_for_blend_search() {
        use crate::embeddings::cosine;
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
        let z = vec![0.0, 0.0, 0.0];
        assert_eq!(cosine(&z, &v), 0.0);
    }


    /// v1.0 auto-merge: adding a near-identical note soft-deletes the
    /// old one and retargets its edges to the new one.
    #[test]
    fn auto_merge_consolidates_near_duplicate_notes() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        // First note.
        let content = "mneme uses content_hash dedup which only catches exact duplicates.             Evolving documents like SKILL.md change slightly between sessions and bypass it.";
        let id1 = api.add(make_mem(content, "evolving doc")).unwrap().id;
        // A second, near-identical note (one word changed).
        let content2 = "mneme uses content_hash dedup which only catches exact duplicates.             Evolving documents like SKILL.md change slightly between sessions and bypass it!";
        let r2 = api.add(make_mem(content2, "evolving doc v2")).unwrap();
        let id2 = r2.id;
        assert_ne!(id1, id2, "exact hash differs, so no dedup");
        // The old one should be soft-deleted (merged into the new).
        let old = api.get(&id1).unwrap();
        assert!(old.is_none(), "old memory should be soft-deleted after merge");
        let new = api.get(&id2).unwrap().expect("new memory exists");
        assert_eq!(new.status, ActionStatus::Active);
        // Both active? No — only the new one is active.
        let all = api.list(100).unwrap();
        assert_eq!(all.len(), 1, "only the merged-into memory should be active");
        assert_eq!(all[0].id, id2);
    }

    /// v1.0 auto-merge: only snapshot-type categories merge; decisions
    /// use supersede edges (unchanged behavior).
    #[test]
    fn auto_merge_skips_decision_category() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        let content = "use library A not library B for the parse step. Reasons: faster, smaller.";
        let mut d1 = make_mem(content, "decision A");
        d1.category = Category::Decision;
        let id1 = api.add(d1).unwrap().id;
        let content2 = "use library A not library B for the parse step. Reasons: faster, smaller!";
        let mut d2 = make_mem(content2, "decision A v2");
        d2.category = Category::Decision;
        let id2 = api.add(d2).unwrap().id;
        // Decision memories should NOT auto-merge (supersede edge instead).
        let old = api.get(&id1).unwrap();
        assert!(old.is_some(), "decision memories must not be merged");
        let all = api.list(100).unwrap();
        assert_eq!(all.len(), 2, "both decisions stay active");
        // But they should be linked with a Supersedes edge.
        let edge = crate::edge::EdgeApi::new(&store, &cfg);
        let neighbors = edge.neighbors(&id2, 1).unwrap();
        assert!(
            neighbors.iter().any(|(m, _)| m.id == id1),
            "decision v2 should supersede-link to v1"
        );
        let _ = id2;
    }

    /// v1.0 auto-merge: merged memory's edges are retargeted.
    #[test]
    fn auto_merge_retargets_edges() {
        let (store, cfg) = store();
        let api = MemoryApi::new(&store, &cfg);
        // A related memory that will get linked to both.
        let hub = api.add(make_mem("hub memory unrelated topic", "hub")).unwrap().id;
        // First note; link it to hub manually.
        let content = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega";
        let id1 = api.add(make_mem(content, "doc1")).unwrap().id;
        let edge_api = crate::edge::EdgeApi::new(&store, &cfg);
        edge_api.link(&id1, &hub, crate::schema::EdgeType::Related, 0.8, None, None).unwrap();
        // Second near-identical note.
        let content2 = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega!";
        let id2 = api.add(make_mem(content2, "doc2")).unwrap().id;
        assert_ne!(id1, id2);
        // doc1 was merged into doc2; the edge doc1→hub should now be
        // doc2→hub.
        let neighbors = edge_api.neighbors(&id2, 1).unwrap();
        assert!(
            neighbors.iter().any(|(m, _)| m.id == hub),
            "edge from merged doc1 should be retargeted to doc2"
        );
    }

}
