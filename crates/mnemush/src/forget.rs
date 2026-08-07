//! Forgetting mechanism.
//!
//! Single simplified formula (see ARCHITECTURE.md):
//!   confidence(t) = initial_confidence
//!                   * 0.5 ^ (days_since_access / effective_half_life)
//!                   * (1 + ln(access_count + 1) * access_boost_factor)
//!
//! where effective_half_life = base * (1 - w + 2*w*importance).
//!
//! Active pruning: confidence < threshold → shadow-delete; recovered
//! within 30 days via `unsoft_delete`.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::config::Config;
use crate::schema::{Edge, Memory};
use crate::store::Store;
use crate::Result;

/// Compute current confidence for a memory at `now`.
pub fn current_confidence(m: &Memory, cfg: &Config, now: DateTime<Utc>) -> f32 {
    if m.never_decay || matches!(m.memory_type, crate::schema::MemoryType::Identity) {
        return 1.0;
    }
    if cfg.forgetting.disable_forgetting {
        return m.confidence;
    }

    let days = (now - m.last_accessed_at).num_days().max(0) as f32;
    let hl = effective_half_life(m, cfg);
    let time_decay = 0.5_f32.powf(days / hl);
    let access_boost =
        1.0 + (m.access_count as f32 + 1.0).ln().max(0.0) * cfg.forgetting.access_boost_factor;
    (m.initial_confidence * time_decay * access_boost).clamp(0.0, 1.0)
}

/// Effective half-life for a memory, factoring in its importance override.
pub fn effective_half_life(m: &Memory, cfg: &Config) -> f32 {
    if let Some(hl) = m.override_half_life {
        return hl;
    }
    let w = cfg.forgetting.half_life_importance_weight.clamp(0.0, 1.0);
    let factor = 1.0 - w + 2.0 * w * m.importance.clamp(0.0, 1.0);
    (cfg.forgetting.half_life_days * factor).max(0.5)
}

/// Boost a memory's stability and confidence on access.
pub fn on_access(m: &mut Memory, cfg: &Config, now: DateTime<Utc>) {
    m.access_count = m.access_count.saturating_add(1);
    m.last_accessed_at = now;
    m.confidence = current_confidence(m, cfg, now);
}

/// Compute current strength for an edge at `now`.
///
/// Same shape as `current_confidence` but for graph edges:
///   strength = initial_strength * 0.5^(days / stability)
///                       * (1 + ln(access+1) * access_boost_factor)
/// clamped to [0, 1]. Honors `disable_forgetting` (frozen).
pub fn current_edge_strength(e: &Edge, cfg: &Config, now: DateTime<Utc>) -> f32 {
    if cfg.forgetting.disable_forgetting {
        return e.strength;
    }
    let last = e.last_activated.unwrap_or(e.created_at);
    let days = (now - last).num_days().max(0) as f32;
    let hl = e.stability.max(0.5);
    let time_decay = 0.5_f32.powf(days / hl);
    let access_boost =
        1.0 + (e.access_count as f32 + 1.0).ln().max(0.0) * cfg.forgetting.access_boost_factor;
    (e.initial_strength * time_decay * access_boost).clamp(0.0, 1.0)
}

/// Boost an edge's strength on activation (neighbor expansion hit).
pub fn on_edge_access(e: &mut Edge, cfg: &Config, now: DateTime<Utc>) {
    e.access_count = e.access_count.saturating_add(1);
    e.last_activated = Some(now);
    e.strength = current_edge_strength(e, cfg, now);
}

/// Process the needs_review queue: for every active memory with
/// `needs_review=true` and `created_at` older than the grace period,
/// clear the flag (mark as reviewed) and downgrade importance by 0.1
/// if the category is `failure`. Idempotent and bounded — never grows
/// the queue unboundedly. Returns the count processed.
///
/// Why downgrading matters: error captures (after_tool_call hook)
/// repeatedly fire on the same recurring failure, inflating the queue.
/// Each processed pass reduces importance, so after a few sessions a
/// repeated error fades out naturally via standard decay.
pub fn process_needs_review(store: &mut Store, grace: ChronoDuration) -> Result<usize> {
    let now = Utc::now();
    let cutoff = (now - grace).timestamp();

    // Find active needs_review rows older than cutoff
    let mut stmt = store.conn.prepare(
        "SELECT id, category, importance FROM memory
         WHERE needs_review = 1
           AND deleted_at IS NULL
           AND created_at <= ?1",
    )?;
    let rows: Vec<(String, String, f32)> = stmt
        .query_map([cutoff], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f32>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    if rows.is_empty() {
        return Ok(0);
    }

    let tx = store.conn.unchecked_transaction()?;
    let mut processed = 0usize;
    for (id, category, importance) in rows {
        // Clear the flag
        tx.execute(
            "UPDATE memory SET needs_review = 0 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        // Downgrade repeated failures
        if category == "failure" {
            let new_imp = (importance - 0.1).max(0.0);
            tx.execute(
                "UPDATE memory SET importance = ?1 WHERE id = ?2",
                rusqlite::params![new_imp, id],
            )?;
        }
        store.log_event_tx(
            &tx,
            "needs_review_processed",
            Some(&id),
            None,
            Some(&format!(
                r#"{{"category":"{}","importance_was":{}}}"#,
                category, importance
            )),
            "background",
        )?;
        processed += 1;
    }
    tx.commit()?;
    Ok(processed)
}

/// Apply Ebbinghaus decay to every active edge in the store.
///
/// Reads all `WHERE deleted_at IS NULL` rows, recomputes `strength`
/// via `current_edge_strength`, and writes back. Writes are wrapped
/// in a single transaction so partial failures don't half-decay the
/// graph. Returns the number of rows updated.
pub fn decay_all_edges(store: &mut Store, cfg: &Config, now: DateTime<Utc>) -> Result<usize> {
    let mut stmt = store.conn.prepare(
        "SELECT id, strength, initial_strength, access_count, last_activated, stability, created_at
         FROM memory_edge WHERE deleted_at IS NULL",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, f32, f32, u32, Option<i64>, f32, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, u32>(3)?,
                r.get(4)?,
                r.get::<_, f32>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    if rows.is_empty() {
        return Ok(0);
    }

    let tx = store.conn.unchecked_transaction()?;
    let mut updated = 0usize;
    let now_ts = now.timestamp();
    for (
        id,
        _strength,
        initial_strength,
        access_count,
        last_activated_ts,
        stability,
        created_at_ts,
    ) in rows
    {
        let edge_for_calc = Edge {
            id: id.clone(),
            source_id: String::new(),
            target_id: String::new(),
            edge_type: crate::schema::EdgeType::Related, // unused by formula
            strength: 0.0,
            initial_strength,
            bidirectional: false,
            provenance: None,
            evidence: None,
            context: None,
            access_count,
            last_activated: last_activated_ts.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
            stability,
            created_at: DateTime::<Utc>::from_timestamp(created_at_ts, 0).unwrap_or(now),
            deleted_at: None,
        };
        let new_strength = current_edge_strength(&edge_for_calc, cfg, now);
        tx.execute(
            "UPDATE memory_edge SET strength = ?1 WHERE id = ?2",
            rusqlite::params![new_strength, id],
        )?;
        updated += 1;
    }
    tx.commit()?;
    let _ = now_ts; // silence unused if compiler ever warns
    Ok(updated)
}

/// Should this memory be pruned?
pub fn should_prune(m: &Memory, cfg: &Config, now: DateTime<Utc>) -> bool {
    if m.never_prune || matches!(m.memory_type, crate::schema::MemoryType::Identity) {
        return false;
    }
    if cfg.forgetting.disable_forgetting {
        return false;
    }
    if m.importance >= cfg.forgetting.prune_importance_exempt {
        return false;
    }

    let conf = current_confidence(m, cfg, now);
    let days_no_access = (now - m.last_accessed_at).num_days();

    if conf < cfg.forgetting.prune_confidence_threshold {
        return true;
    }
    if conf < cfg.forgetting.prune_min_confidence_for_candidate
        && days_no_access > cfg.forgetting.prune_max_days_no_access
    {
        return true;
    }
    false
}

/// Why a memory was selected for pruning. Surfaced in `memory_event.details`
/// so future sessions can audit *why* a memory was removed.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PruneReason {
    /// `should_prune` returned true because confidence dropped below
    /// `prune_confidence_threshold` (default 0.1).
    LowConfidence {
        confidence: f32,
        threshold: f32,
        days_no_access: i64,
    },
    /// `should_prune` returned true because confidence < `prune_min_confidence_for_candidate`
    /// AND no access for `prune_max_days_no_access` days.
    Stale {
        confidence: f32,
        threshold: f32,
        days_no_access: i64,
    },
    /// Two-step isolation: already soft-deleted, grace elapsed, zero inbound
    /// edges, importance below `max_importance`, last access long ago.
    Isolated {
        grace_days: i64,
        inbound_edges: i64,
        importance: f32,
        max_importance: f32,
        days_no_access: i64,
    },
}

/// List memories that `should_prune` would currently flag. Read-only.
pub fn prune_dry_run(
    store: &Store,
    cfg: &Config,
    now: DateTime<Utc>,
    limit: Option<usize>,
) -> Result<Vec<(Memory, PruneReason)>> {
    let mut stmt = store
        .conn
        .prepare("SELECT * FROM memory WHERE deleted_at IS NULL ORDER BY last_accessed_at ASC")?;
    let rows = stmt.query_map([], Store::row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        let m = r?;
        if !should_prune(&m, cfg, now) {
            continue;
        }
        let conf = current_confidence(&m, cfg, now);
        let days_no_access = (now - m.last_accessed_at).num_days();
        let reason = if conf < cfg.forgetting.prune_confidence_threshold {
            PruneReason::LowConfidence {
                confidence: conf,
                threshold: cfg.forgetting.prune_confidence_threshold,
                days_no_access,
            }
        } else {
            PruneReason::Stale {
                confidence: conf,
                threshold: cfg.forgetting.prune_min_confidence_for_candidate,
                days_no_access,
            }
        };
        out.push((m, reason));
        if let Some(n) = limit {
            if out.len() >= n {
                break;
            }
        }
    }
    Ok(out)
}

/// Apply soft delete to all prune candidates. Returns `(id, reason)` for each
/// memory that was soft-deleted. Each deletion also writes a
/// `memory_prune` event with the serialized reason.
pub fn prune_apply(
    store: &mut Store,
    cfg: &Config,
    now: DateTime<Utc>,
    limit: Option<usize>,
) -> Result<Vec<(String, PruneReason)>> {
    let candidates = prune_dry_run(store, cfg, now, limit)?;
    let mut deleted = Vec::with_capacity(candidates.len());
    let now_ts = now.timestamp();
    for (m, reason) in candidates {
        let reason_json =
            serde_json::to_string(&reason).unwrap_or_else(|_| r#"{"kind":"unknown"}"#.to_string());
        let tx = store.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE memory SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![now_ts, m.id],
        )?;
        store.log_event_tx(
            &tx,
            "memory_prune",
            Some(&m.id),
            None,
            Some(&reason_json),
            "forget",
        )?;
        tx.commit()?;
        deleted.push((m.id, reason));
    }
    Ok(deleted)
}

/// Parameters for isolation-based hard delete. Bundled to keep
/// `isolate_hard_delete` / `isolate_dry_run` signatures under the 7-arg
/// clippy limit.
#[derive(Debug, Clone, Copy)]
pub struct IsolateOpts {
    /// Days after soft-delete before hard-delete is allowed.
    pub grace_days: i64,
    /// Skip memories with importance ≥ this (caller's filter; default 0.5).
    pub max_importance: f32,
    /// Skip memories accessed within the last N days.
    pub min_days_no_access: i64,
    /// Cap the number of candidates considered.
    pub limit: Option<usize>,
}

impl Default for IsolateOpts {
    fn default() -> Self {
        Self {
            grace_days: 7,
            max_importance: 0.5,
            min_days_no_access: 30,
            limit: None,
        }
    }
}

/// Dry-run: list memories that would be hard-deleted by `isolate_hard_delete`.
/// Uses the same eligibility SQL but never commits a delete.
pub fn isolate_dry_run(
    store: &Store,
    now: DateTime<Utc>,
    opts: IsolateOpts,
) -> Result<Vec<Memory>> {
    let now_ts = now.timestamp();
    let grace_cutoff = now_ts - opts.grace_days * 86_400;
    let access_cutoff_ts = now_ts - opts.min_days_no_access * 86_400;

    let mut sql = String::from(
        r#"SELECT m.*,
                  (SELECT COUNT(*) FROM memory_edge e
                   WHERE e.target_id = m.id AND e.deleted_at IS NULL) AS inbound
           FROM memory m
           WHERE m.deleted_at IS NOT NULL
             AND m.deleted_at <= ?1
             AND m.memory_type != 'identity'
             AND m.never_prune = 0
             AND m.importance < ?2
             AND m.last_accessed_at < ?3
           ORDER BY m.last_accessed_at ASC"#,
    );
    if let Some(n) = opts.limit {
        sql.push_str(&format!(" LIMIT {}", n));
    }
    let mut stmt = store.conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![grace_cutoff, opts.max_importance, access_cutoff_ts],
        |row| {
            // Read the inbound count into a local first; we filter to 0 below.
            let inbound: i64 = row.get("inbound")?;
            if inbound != 0 {
                return Ok(None);
            }
            Ok(Some(Store::row_to_memory(row)?))
        },
    )?;
    let mut out = Vec::new();
    for r in rows {
        if let Ok(Some(m)) = r {
            out.push(m);
        }
    }
    Ok(out)
}

/// Hard-delete soft-deleted memories that are also isolated, low-importance,
/// and stale. Returns `(id, reason)` for each row actually deleted.
pub fn isolate_hard_delete(
    store: &mut Store,
    cfg: &Config,
    now: DateTime<Utc>,
    opts: IsolateOpts,
) -> Result<Vec<(String, PruneReason)>> {
    let _ = cfg; // reserved for future per-config policy
    let now_ts = now.timestamp();
    // grace=0 means "eligible immediately after soft-delete".
    // The `<=` lets a memory soft-deleted *at* `now` qualify.
    let grace_cutoff = now_ts - opts.grace_days * 86_400;
    let access_cutoff_ts = now_ts - opts.min_days_no_access * 86_400;

    let mut sql = String::from(
        r#"SELECT m.id, m.importance, m.last_accessed_at,
                  (SELECT COUNT(*) FROM memory_edge e
                   WHERE e.target_id = m.id AND e.deleted_at IS NULL) AS inbound
           FROM memory m
           WHERE m.deleted_at IS NOT NULL
             AND m.deleted_at <= ?1
             AND m.memory_type != 'identity'
             AND m.never_prune = 0
             AND m.importance < ?2
             AND m.last_accessed_at < ?3
           ORDER BY m.last_accessed_at ASC"#,
    );
    if let Some(n) = opts.limit {
        sql.push_str(&format!(" LIMIT {}", n));
    }
    let mut stmt = store.conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![grace_cutoff, opts.max_importance, access_cutoff_ts],
        |row| {
            let id: String = row.get(0)?;
            let importance: f32 = row.get(1)?;
            let last_accessed_ts: i64 = row.get(2)?;
            let inbound: i64 = row.get(3)?;
            Ok((id, importance, last_accessed_ts, inbound))
        },
    )?;
    let mut candidates = Vec::new();
    for r in rows {
        candidates.push(r?);
    }
    let candidates: Vec<_> = candidates.into_iter().filter(|c| c.3 == 0).collect();

    let mut deleted = Vec::with_capacity(candidates.len());
    for (id, importance, last_accessed_ts, _inbound) in candidates {
        let days_no_access = (now_ts - last_accessed_ts) / 86_400;
        let reason = PruneReason::Isolated {
            grace_days: opts.grace_days,
            inbound_edges: 0,
            importance,
            max_importance: opts.max_importance,
            days_no_access,
        };
        let reason_json =
            serde_json::to_string(&reason).unwrap_or_else(|_| r#"{"kind":"unknown"}"#.to_string());

        let tx = store.conn.unchecked_transaction()?;
        // ponytail: FTS5 rowid alignment was fixed in v0.2 (FTS5 now
        // auto-assigns its own rowid, see store.rs:237). The remaining
        // concern: hard-deleting the memory row leaves the matching
        // FTS5 row orphaned, matching current soft_delete behavior.
        // A future `mnemush vacuum` could rebuild memory_fts if orphans
        // become a problem.
        // Keep the FTS5 index rowid-aligned: capture the rowid BEFORE
        // deleting the memory row, then remove the orphaned FTS row.
        // Without this, rowid reuse made search return wrong content.
        // (2026-08-06 fix.)
        let fts_rowid: Option<i64> = tx
            .query_row("SELECT rowid FROM memory WHERE id = ?1", rusqlite::params![id], |r| r.get(0))
            .optional()?;
        tx.execute("DELETE FROM memory WHERE id = ?1", rusqlite::params![id])?;
        if let Some(rid) = fts_rowid {
            tx.execute("DELETE FROM memory_fts WHERE rowid = ?1", rusqlite::params![rid])?;
        }
        store.log_event_tx(
            &tx,
            "memory_hard_delete",
            Some(&id),
            None,
            Some(&reason_json),
            "forget",
        )?;
        tx.commit()?;
        deleted.push((id, reason));
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ActionStatus, Category, MemoryType, Source, Tier};
    use chrono::Duration;

    fn cfg() -> Config {
        Config::default()
    }

    fn mem(days_old: i64, importance: f32, access_count: u32) -> Memory {
        let now = Utc::now();
        Memory {
            id: "x".into(),
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            category: Category::Note,
            title: "t".into(),
            content: "c".into(),
            context: None,
            topic_key: None,
            tags: vec![],
            project: None,
            source: Source::Manual,
            initial_confidence: 1.0,
            confidence: 1.0,
            importance,
            access_count,
            last_accessed_at: now - Duration::days(days_old),
            created_at: now - Duration::days(days_old),
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            content_hash: "h".into(),
            deleted_at: None,
            needs_review: false,
            status: ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
        }
    }

    #[test]
    fn fresh_memory_has_full_confidence() {
        let m = mem(0, 0.5, 0);
        let c = current_confidence(&m, &cfg(), Utc::now());
        assert!((c - 1.0).abs() < 0.01, "fresh should be ~1.0, got {}", c);
    }

    #[test]
    fn old_memory_decays() {
        let m = mem(180, 0.5, 0);
        let c = current_confidence(&m, &cfg(), Utc::now());
        assert!(
            c < 0.5,
            "180 days old should decay significantly, got {}",
            c
        );
    }

    #[test]
    fn high_importance_decays_slower() {
        let low = mem(180, 0.0, 0);
        let high = mem(180, 1.0, 0);
        let cfg = cfg();
        let c_low = current_confidence(&low, &cfg, Utc::now());
        let c_high = current_confidence(&high, &cfg, Utc::now());
        assert!(c_high > c_low);
    }

    #[test]
    fn high_access_count_boosts() {
        let m0 = mem(30, 0.5, 0);
        let m10 = mem(30, 0.5, 10);
        let cfg = cfg();
        let c0 = current_confidence(&m0, &cfg, Utc::now());
        let c10 = current_confidence(&m10, &cfg, Utc::now());
        assert!(c10 > c0, "more access should boost, got {} vs {}", c0, c10);
    }

    #[test]
    fn never_decay_returns_one() {
        let mut m = mem(1000, 0.5, 0);
        m.never_decay = true;
        assert_eq!(current_confidence(&m, &cfg(), Utc::now()), 1.0);
    }

    #[test]
    fn identity_never_decays() {
        let mut m = mem(1000, 0.5, 0);
        m.memory_type = MemoryType::Identity;
        assert_eq!(current_confidence(&m, &cfg(), Utc::now()), 1.0);
    }

    #[test]
    fn pruning_threshold() {
        let m = mem(365, 0.0, 0);
        assert!(should_prune(&m, &cfg(), Utc::now()));
    }

    #[test]
    fn important_memory_exempt() {
        let m = mem(365, 0.9, 0);
        assert!(!should_prune(&m, &cfg(), Utc::now()));
    }

    #[test]
    fn never_prune_skips() {
        let mut m = mem(1000, 0.0, 0);
        m.never_prune = true;
        assert!(!should_prune(&m, &cfg(), Utc::now()));
    }

    // ── prune_dry_run / prune_apply / isolate_hard_delete ──────────

    /// Insert a memory with explicit (id, days_since_access, importance).
    /// The memory is active (deleted_at=NULL), and its confidence is set
    /// to whatever the decay formula yields at `now`.
    fn insert_test_memory(store: &mut Store, id: &str, days_old: i64, importance: f32) {
        let now = Utc::now();
        let mut m = mem(days_old, importance, 0);
        m.id = id.to_string();
        m.content_hash = format!("h-{}", id);
        // Force confidence to match what should_prune sees, so the test
        // scenario is independent of test-runner wall time.
        m.confidence = current_confidence(&m, &Config::default(), now);
        m.last_accessed_at = now - Duration::days(days_old);
        m.created_at = now - Duration::days(days_old);
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn dry_run_finds_old_low_importance() {
        let mut store = Store::open_in_memory().unwrap();
        insert_test_memory(&mut store, "old-junk", 365, 0.0);
        insert_test_memory(&mut store, "fresh", 0, 0.5);
        insert_test_memory(&mut store, "important", 365, 0.9); // exempt
                                                               // 200d old, importance 0.3: conf ≈ 0.5^(200/72) ≈ 0.15 < 0.3 → stale.
        insert_test_memory(&mut store, "mid-stale", 200, 0.3);

        let hits = prune_dry_run(&store, &Config::default(), Utc::now(), None).unwrap();
        let ids: Vec<&str> = hits.iter().map(|(m, _)| m.id.as_str()).collect();
        assert!(ids.contains(&"old-junk"), "expected old-junk in {:?}", ids);
        assert!(
            ids.contains(&"mid-stale"),
            "expected mid-stale in {:?}",
            ids
        );
        assert!(!ids.contains(&"fresh"));
        assert!(!ids.contains(&"important"));
    }

    #[test]
    fn dry_run_respects_limit() {
        let mut store = Store::open_in_memory().unwrap();
        for i in 0..5 {
            insert_test_memory(&mut store, &format!("m-{}", i), 365, 0.0);
        }
        let hits = prune_dry_run(&store, &Config::default(), Utc::now(), Some(2)).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn apply_soft_deletes_and_logs_event() {
        let mut store = Store::open_in_memory().unwrap();
        insert_test_memory(&mut store, "doomed", 365, 0.0);
        insert_test_memory(&mut store, "safe", 0, 0.5);

        let n = Utc::now();
        let deleted = prune_apply(&mut store, &Config::default(), n, None).unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, "doomed");

        // soft-deleted: deleted_at is set
        let n_doomed: Option<i64> = store
            .conn
            .query_row("SELECT deleted_at FROM memory WHERE id='doomed'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(n_doomed.is_some(), "doomed should be soft-deleted");

        // event log: one memory_prune row
        let n_events: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_event WHERE event_type='memory_prune'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_events, 1);

        // safe is untouched
        let safe_deleted: Option<i64> = store
            .conn
            .query_row("SELECT deleted_at FROM memory WHERE id='safe'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(safe_deleted.is_none());
    }

    #[test]
    fn isolate_requires_soft_delete_first() {
        let mut store = Store::open_in_memory().unwrap();
        // Active memory (not soft-deleted) — should NOT be hard-deleted
        // even if it meets all other criteria.
        insert_test_memory(&mut store, "still-active", 365, 0.1);

        let n = Utc::now();
        let deleted = isolate_hard_delete(
            &mut store,
            &Config::default(),
            n,
            IsolateOpts {
                grace_days: 0,
                max_importance: 0.5,
                min_days_no_access: 0,
                limit: None,
            },
        )
        .unwrap();
        assert!(
            deleted.is_empty(),
            "active memory should not be hard-deleted, got {:?}",
            deleted
        );
    }

    #[test]
    fn isolate_respects_grace_period() {
        let mut store = Store::open_in_memory().unwrap();
        // Soft-deleted memory, but only 2 days ago.
        let now = Utc::now();
        let mut m = mem(60, 0.1, 0);
        m.id = "too-recent".into();
        m.content_hash = "h-1".into();
        m.deleted_at = Some(now - Duration::days(2));
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
        drop(m);

        let deleted = isolate_hard_delete(
            &mut store,
            &Config::default(),
            now,
            IsolateOpts {
                grace_days: 7,
                max_importance: 0.5,
                min_days_no_access: 30,
                limit: None,
            },
        )
        .unwrap();
        assert!(deleted.is_empty(), "within grace should not be deleted");

        // Now extend the soft-delete age to 10d (> grace) and retry.
        let past = (now - Duration::days(10)).timestamp();
        store
            .conn
            .execute(
                "UPDATE memory SET deleted_at=?1 WHERE id='too-recent'",
                rusqlite::params![past],
            )
            .unwrap();
        let deleted = isolate_hard_delete(
            &mut store,
            &Config::default(),
            now,
            IsolateOpts {
                grace_days: 7,
                max_importance: 0.5,
                min_days_no_access: 30,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, "too-recent");
    }

    #[test]
    fn isolate_skips_memories_with_inbound_edges() {
        let mut store = Store::open_in_memory().unwrap();
        let now = Utc::now();
        // Two isolated soft-deleted memories
        for (id, age) in [("orphan-1", 60), ("orphan-2", 60)] {
            let mut m = mem(age, 0.1, 0);
            m.id = id.into();
            m.content_hash = format!("h-{}", id);
            m.deleted_at = Some(now - Duration::days(20));
            let tx = store.conn.unchecked_transaction().unwrap();
            Store::insert_memory_tx(&tx, &m).unwrap();
            tx.commit().unwrap();
        }
        // A real source memory + a linked target that has an inbound edge
        // (so it should NOT be hard-deleted).
        let mut src = mem(0, 0.5, 0);
        src.id = "src".into();
        src.content_hash = "h-src".into();
        let mut tgt = mem(60, 0.1, 0);
        tgt.id = "linked".into();
        tgt.content_hash = "h-linked".into();
        tgt.deleted_at = Some(now - Duration::days(20));
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &src).unwrap();
        Store::insert_memory_tx(&tx, &tgt).unwrap();
        tx.execute(
            "INSERT INTO memory_edge (id, source_id, target_id, edge_type, created_at) \
             VALUES ('e1', 'src', 'linked', 'related', ?1)",
            rusqlite::params![now.timestamp()],
        )
        .unwrap();
        tx.commit().unwrap();
        drop(src);
        drop(tgt);

        let deleted = isolate_hard_delete(
            &mut store,
            &Config::default(),
            now,
            IsolateOpts {
                grace_days: 7,
                max_importance: 0.5,
                min_days_no_access: 30,
                limit: None,
            },
        )
        .unwrap();
        let ids: Vec<&str> = deleted.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"orphan-1"));
        assert!(ids.contains(&"orphan-2"));
        assert!(!ids.contains(&"linked"), "linked should be spared");
    }

    #[test]
    fn isolate_writes_memory_hard_delete_event() {
        let mut store = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let mut m = mem(60, 0.1, 0);
        m.id = "victim".into();
        m.content_hash = "h-victim".into();
        m.deleted_at = Some(now - Duration::days(20));
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
        drop(m);

        let deleted = isolate_hard_delete(
            &mut store,
            &Config::default(),
            now,
            IsolateOpts {
                grace_days: 7,
                max_importance: 0.5,
                min_days_no_access: 30,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(deleted.len(), 1);

        let (ev_type, details): (String, Option<String>) = store
            .conn
            .query_row(
                "SELECT event_type, details FROM memory_event \
                 WHERE event_type='memory_hard_delete' AND memory_id='victim'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ev_type, "memory_hard_delete");
        let details = details.unwrap();
        assert!(
            details.contains("\"kind\":\"isolated\""),
            "got: {}",
            details
        );
    }

    #[test]
    fn end_to_end_two_step() {
        // The realistic scenario: a 365-day-old, importance 0.1 memory
        // 1) is NOT yet prunable (fresh data); 2) ages via "advance time"
        // by mutating last_accessed_at; 3) prune_dry_run finds it;
        // 4) prune_apply soft-deletes; 5) isolate (with grace=0) hard-deletes.
        let mut store = Store::open_in_memory().unwrap();
        let mut m = mem(400, 0.1, 0);
        m.id = "stale-note".into();
        m.content_hash = "h-stale".into();
        let tx = store.conn.unchecked_transaction().unwrap();
        Store::insert_memory_tx(&tx, &m).unwrap();
        tx.commit().unwrap();
        drop(m);

        // Step 1: dry-run finds it.
        let hits = prune_dry_run(&store, &Config::default(), Utc::now(), None).unwrap();
        assert!(hits.iter().any(|(m, _)| m.id == "stale-note"));

        // Step 2: apply soft-deletes.
        let deleted = prune_apply(&mut store, &Config::default(), Utc::now(), None).unwrap();
        assert!(deleted.iter().any(|(id, _)| id == "stale-note"));

        // Step 3: hard-delete (grace=0 means immediately eligible).
        let n = Utc::now();
        let hard = isolate_hard_delete(
            &mut store,
            &Config::default(),
            n,
            IsolateOpts {
                grace_days: 0,
                max_importance: 0.5,
                min_days_no_access: 30,
                limit: None,
            },
        )
        .unwrap();
        assert!(hard.iter().any(|(id, _)| id == "stale-note"));

        // Verify the row is gone.
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory WHERE id='stale-note'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    // ── edge decay tests ───────────────────────────────────────────────────

    fn edge(days_old: i64, initial_strength: f32, access_count: u32) -> Edge {
        let now = Utc::now();
        Edge {
            id: "e1".into(),
            source_id: "a".into(),
            target_id: "b".into(),
            edge_type: crate::schema::EdgeType::Related,
            strength: initial_strength,
            initial_strength,
            bidirectional: true,
            provenance: None,
            evidence: None,
            context: None,
            access_count,
            last_activated: Some(now - Duration::days(days_old)),
            stability: 60.0,
            created_at: now - Duration::days(days_old),
            deleted_at: None,
        }
    }

    #[test]
    fn edge_strength_decays_over_time() {
        let c = cfg();
        let now = Utc::now();
        let e = edge(60, 0.8, 0);
        let s = current_edge_strength(&e, &c, now);
        // After 1 half-life (60 days) with no access, strength should be
        // ~0.5 * initial_strength (time decay factor), with the same
        // access_boost scaling as memory confidence.
        let expected =
            (0.8 * 0.5 * (1.0 + 1.0_f32.ln() * c.forgetting.access_boost_factor)).clamp(0.0, 1.0);
        assert!((s - expected).abs() < 0.05, "expected ~{expected}, got {s}");
    }

    #[test]
    fn edge_access_boost_increases_strength() {
        let c = cfg();
        let now = Utc::now();
        let unused = edge(30, 0.5, 0);
        let used = edge(30, 0.5, 10);
        let s_unused = current_edge_strength(&unused, &c, now);
        let s_used = current_edge_strength(&used, &c, now);
        assert!(
            s_used > s_unused,
            "used {s_used} should beat unused {s_unused}"
        );
    }

    #[test]
    fn edge_strength_clamped_to_unit_interval() {
        let c = cfg();
        let now = Utc::now();
        // Very fresh, very heavily accessed — should not exceed 1.0
        let e = edge(0, 0.8, 1000);
        let s = current_edge_strength(&e, &c, now);
        assert!(s <= 1.0, "strength {s} exceeded 1.0");
        assert!(s > 0.0, "strength should be > 0");
    }

    #[test]
    fn disable_forgetting_freezes_edge_strength() {
        let mut c = cfg();
        c.forgetting.disable_forgetting = true;
        let now = Utc::now();
        let e = edge(365, 0.7, 0);
        let s = current_edge_strength(&e, &c, now);
        // disable_forgetting should return current strength unchanged
        assert!((s - 0.7).abs() < 0.01, "expected 0.7, got {s}");
    }

    #[test]
    fn on_edge_access_refreshes_strength() {
        let c = cfg();
        let now = Utc::now();
        let mut e = edge(120, 0.5, 0);
        // Pretend it just got accessed via neighbors (last_activated updated)
        on_edge_access(&mut e, &c, now);
        assert_eq!(e.access_count, 1);
        assert!((e.last_activated.unwrap() - now).num_seconds().abs() < 2);
        // After access, strength should be > old decayed strength
        let fresh = current_edge_strength(&e, &c, now);
        let decayed = {
            let mut old = e.clone();
            old.last_activated = Some(now - Duration::days(120));
            old.access_count = 0;
            current_edge_strength(&old, &c, now)
        };
        assert!(fresh > decayed);
    }

    fn insert_test_edge(store: &mut Store, id: &str, days_old: i64, initial: f32) {
        let now = Utc::now();
        let ts = (now - Duration::days(days_old)).timestamp();
        // UNIQUE (source_id, target_id, edge_type) constraint + FK to memory.
        // Derive memory IDs from the edge ID so multiple edges coexist.
        let src = format!("{id}-src");
        let tgt = format!("{id}-tgt");
        for (mid, mtitle, mcontent) in [(&src, "ts", "cs"), (&tgt, "tt", "ct")] {
            store
                .conn
                .execute(
                    "INSERT OR IGNORE INTO memory (id, memory_type, tier, category, title,
                        content, source, initial_confidence, confidence, importance,
                        access_count, last_accessed_at, created_at, content_hash, needs_review)
                     VALUES (?1, 'semantic', 'global', 'note', ?2, ?3, 'manual',
                             1.0, 1.0, 0.5, 0, ?4, ?4, ?5, 0)",
                    rusqlite::params![mid, mtitle, mcontent, ts, format!("h-{mid}")],
                )
                .unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO memory_edge (id, source_id, target_id, edge_type,
                    strength, initial_strength, bidirectional,
                    access_count, last_activated, stability, created_at)
                 VALUES (?1, ?2, ?3, 'related', ?4, ?4, 1, 0, ?5, 60.0, ?5)",
                rusqlite::params![id, src, tgt, initial, ts],
            )
            .unwrap();
    }

    #[test]
    fn decay_all_edges_updates_strength_in_db() {
        let mut store = Store::open_in_memory().unwrap();
        // Old edge (120d) — should drop substantially
        insert_test_edge(&mut store, "e-old", 120, 0.8);
        // Fresh edge (1d) — should stay roughly the same
        insert_test_edge(&mut store, "e-fresh", 1, 0.6);

        let now = Utc::now();
        let updated = decay_all_edges(&mut store, &cfg(), now).unwrap();
        assert_eq!(updated, 2);

        // Old edge: after 2 half-lives (120/60) decay factor = 0.25
        let old_strength: f32 = store
            .conn
            .query_row(
                "SELECT strength FROM memory_edge WHERE id = 'e-old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let calc_old = current_edge_strength(&edge(120, 0.8, 0), &cfg(), now);
        assert!(
            (old_strength - calc_old).abs() < 0.01,
            "old: stored={old_strength} calc={calc_old}"
        );
        assert!(
            old_strength < 0.4,
            "expected heavy decay, got {old_strength}"
        );

        // Fresh edge: should be close to initial
        let fresh_strength: f32 = store
            .conn
            .query_row(
                "SELECT strength FROM memory_edge WHERE id = 'e-fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let calc_fresh = current_edge_strength(&edge(1, 0.6, 0), &cfg(), now);
        assert!(
            (fresh_strength - calc_fresh).abs() < 0.01,
            "fresh: stored={fresh_strength} calc={calc_fresh}"
        );
        assert!(
            fresh_strength > 0.55,
            "expected minimal decay, got {fresh_strength}"
        );
    }

    #[test]
    fn decay_all_edges_returns_zero_for_empty_db() {
        let mut store = Store::open_in_memory().unwrap();
        let updated = decay_all_edges(&mut store, &cfg(), Utc::now()).unwrap();
        assert_eq!(updated, 0);
    }

    #[test]
    fn decay_all_edges_skips_deleted_rows() {
        let mut store = Store::open_in_memory().unwrap();
        insert_test_edge(&mut store, "e-live", 60, 0.5);
        // Create the memory rows for the deleted edge, then insert a soft-deleted edge.
        let now = Utc::now();
        let ts = now.timestamp();
        store
            .conn
            .execute(
                "INSERT OR IGNORE INTO memory (id, memory_type, tier, category, title,
                    content, source, initial_confidence, confidence, importance,
                    access_count, last_accessed_at, created_at, content_hash, needs_review)
                 VALUES ('e-dead-src', 'semantic', 'global', 'note', 't', 'c', 'manual',
                         1.0, 1.0, 0.5, 0, ?1, ?1, 'h', 0)",
                rusqlite::params![ts],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT OR IGNORE INTO memory (id, memory_type, tier, category, title,
                    content, source, initial_confidence, confidence, importance,
                    access_count, last_accessed_at, created_at, content_hash, needs_review)
                 VALUES ('e-dead-tgt', 'semantic', 'global', 'note', 't', 'c', 'manual',
                         1.0, 1.0, 0.5, 0, ?1, ?1, 'h', 0)",
                rusqlite::params![ts],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO memory_edge (id, source_id, target_id, edge_type,
                    strength, initial_strength, bidirectional,
                    access_count, last_activated, stability, created_at, deleted_at)
                 VALUES ('e-dead', 'e-dead-src', 'e-dead-tgt', 'related', 0.9, 0.9, 1, 0, ?1, 60.0, ?1, 1)",
                rusqlite::params![ts],
            )
            .unwrap();
        let updated = decay_all_edges(&mut store, &cfg(), Utc::now()).unwrap();
        assert_eq!(updated, 1); // only e-live
        let dead_strength: f32 = store
            .conn
            .query_row(
                "SELECT strength FROM memory_edge WHERE id = 'e-dead'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (dead_strength - 0.9).abs() < 0.001,
            "deleted row should not be touched"
        );
    }

    // ── process_needs_review tests ────────────────────────────────────────

    fn insert_needs_review(store: &mut Store, id: &str, days_old: i64) {
        let now = Utc::now();
        let ts = (now - Duration::days(days_old)).timestamp();
        store
            .conn
            .execute(
                "INSERT INTO memory (id, memory_type, tier, category, title,
                    content, source, initial_confidence, confidence, importance,
                    access_count, last_accessed_at, created_at, content_hash, needs_review)
                 VALUES (?1, 'semantic', 'global', 'failure', ?1, 'err', 'auto_heuristic',
                         1.0, 0.5, 0.7, 0, ?2, ?2, ?3, 1)",
                rusqlite::params![id, ts, format!("h-{id}")],
            )
            .unwrap();
    }

    #[test]
    fn process_needs_review_clears_old_flag() {
        let mut store = Store::open_in_memory().unwrap();
        insert_needs_review(&mut store, "old-err", 3);
        insert_needs_review(&mut store, "fresh-err", 0);
        let n = process_needs_review(&mut store, Duration::days(1)).unwrap();
        assert_eq!(n, 1); // only old-err is past grace
        let old_flag: i64 = store
            .conn
            .query_row(
                "SELECT needs_review FROM memory WHERE id='old-err'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let fresh_flag: i64 = store
            .conn
            .query_row(
                "SELECT needs_review FROM memory WHERE id='fresh-err'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_flag, 0, "old should be cleared");
        assert_eq!(fresh_flag, 1, "fresh should be left alone");
    }

    #[test]
    fn process_needs_review_downgrades_failure_importance() {
        let mut store = Store::open_in_memory().unwrap();
        insert_needs_review(&mut store, "err", 5);
        process_needs_review(&mut store, Duration::days(1)).unwrap();
        let imp: f32 = store
            .conn
            .query_row("SELECT importance FROM memory WHERE id='err'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Started at 0.7, one pass downgrades by 0.1 → 0.6
        assert!(
            (imp - 0.6).abs() < 0.01,
            "expected importance ~0.6 after one pass, got {imp}"
        );
    }

    #[test]
    fn process_needs_review_returns_zero_when_empty() {
        let mut store = Store::open_in_memory().unwrap();
        let n = process_needs_review(&mut store, Duration::days(1)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn process_needs_review_skips_deleted_rows() {
        let mut store = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let ts = (now - Duration::days(5)).timestamp();
        store
            .conn
            .execute(
                "INSERT INTO memory (id, memory_type, tier, category, title,
                    content, source, initial_confidence, confidence, importance,
                    access_count, last_accessed_at, created_at, content_hash, needs_review, deleted_at)
                 VALUES ('soft-err', 'semantic', 'global', 'failure', 'x', 'x', 'auto_heuristic',
                         1.0, 0.5, 0.7, 0, ?1, ?1, 'h', 1, ?1)",
                rusqlite::params![ts],
            )
            .unwrap();
        let n = process_needs_review(&mut store, Duration::days(1)).unwrap();
        assert_eq!(n, 0, "soft-deleted memories should not be processed");
    }
}
