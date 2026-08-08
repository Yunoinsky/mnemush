//! Edge operations for the LTM graph.

use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use crate::config::Config;
use crate::error::Result;
use crate::schema::{Edge, EdgeType, Memory};
use crate::store::Store;

pub struct EdgeApi<'a> {
    pub store: &'a Store,
    pub config: &'a Config,
}

impl<'a> EdgeApi<'a> {
    pub fn new(store: &'a Store, config: &'a Config) -> Self {
        Self { store, config }
    }

    /// Create or update an edge. Idempotent on (source, target, type).
    pub fn link(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: EdgeType,
        strength: f32,
        provenance: Option<&str>,
        evidence: Option<&str>,
    ) -> Result<Edge> {
        let tx = self.store.conn.unchecked_transaction()?;
        let edge = self.link_in_tx(
            &tx, source_id, target_id, edge_type, strength, provenance, evidence,
        )?;
        tx.commit()?;
        Ok(edge)
    }

    /// In-transaction variant used during memory::add auto-link.
    #[allow(clippy::too_many_arguments)]
    pub fn link_in_tx(
        &self,
        tx: &rusqlite::Transaction,
        source_id: &str,
        target_id: &str,
        edge_type: EdgeType,
        strength: f32,
        provenance: Option<&str>,
        evidence: Option<&str>,
    ) -> Result<Edge> {
        let now = Utc::now();
        let edge = Edge {
            id: Uuid::new_v4().to_string(),
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            edge_type,
            strength: strength.clamp(0.0, 1.0),
            initial_strength: strength.clamp(0.0, 1.0),
            bidirectional: edge_type.default_bidirectional(),
            provenance: provenance.map(str::to_string),
            evidence: evidence.map(str::to_string),
            context: None,
            access_count: 0,
            last_activated: None,
            stability: self.config.edges.edge_decay_half_life_days,
            created_at: now,
            deleted_at: None,
        };
        // Try to insert; on UNIQUE conflict (same source, target, type)
        // update strength to max(existing, new) instead of failing.
        let inserted = tx.execute(
            r#"INSERT OR IGNORE INTO memory_edge (
                id, source_id, target_id, edge_type,
                strength, initial_strength, bidirectional,
                provenance, evidence, context,
                access_count, last_activated, stability,
                created_at, deleted_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
            params![
                edge.id,
                edge.source_id,
                edge.target_id,
                edge.edge_type.as_str(),
                edge.strength,
                edge.initial_strength,
                edge.bidirectional as i32,
                edge.provenance,
                edge.evidence,
                edge.context,
                edge.access_count,
                edge.last_activated.map(|d| d.timestamp()),
                edge.stability,
                edge.created_at.timestamp(),
                edge.deleted_at.map(|d| d.timestamp()),
            ],
        )?;
        if inserted == 0 {
            // existing edge → update strength (take max)
            tx.execute(
                r#"UPDATE memory_edge
                   SET strength = MAX(strength, ?1)
                   WHERE source_id = ?2 AND target_id = ?3 AND edge_type = ?4
                     AND deleted_at IS NULL"#,
                params![
                    edge.strength,
                    edge.source_id,
                    edge.target_id,
                    edge.edge_type.as_str()
                ],
            )?;
        }
        self.store
            .log_event_tx(tx, "edge_link", None, Some(&edge.id), None, "agent")?;
        Ok(edge)
    }

    /// BFS over outgoing edges up to `max_hops` hops. Returns unique
    /// memory ids with the minimum hop distance.
    pub fn neighbors(&self, root_id: &str, max_hops: usize) -> Result<Vec<(Memory, usize)>> {
        let max_hops = max_hops.min(self.config.edges.max_neighbor_hops);
        if max_hops == 0 {
            return Ok(vec![]);
        }
        // Use recursive CTE for BFS.
        let sql = r#"
            WITH RECURSIVE walk(id, depth) AS (
                SELECT ?1, 0
                UNION
                SELECT CASE WHEN e.source_id = w.id THEN e.target_id ELSE e.source_id END,
                       w.depth + 1
                FROM memory_edge e
                JOIN walk w ON (e.source_id = w.id OR (e.bidirectional = 1 AND e.target_id = w.id))
                WHERE w.depth < ?2
                  AND e.deleted_at IS NULL
            )
            SELECT DISTINCT m.*, MIN(w.depth) AS d
            FROM walk w
            JOIN memory m ON m.id = w.id
            WHERE m.id != ?1 AND m.deleted_at IS NULL
            GROUP BY m.id
            ORDER BY d ASC, m.importance DESC
            LIMIT 100
            "#;
        let mut stmt = self.store.conn.prepare(sql)?;
        let rows = stmt.query_map(params![root_id, max_hops as i64], |row| {
            let m = Store::row_to_memory(row)?;
            let d: i64 = row.get("d")?;
            Ok((m, d as usize))
        })?;
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
    use crate::memory::MemoryApi;
    use crate::schema::NewMemory;

    fn setup() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }

    #[test]
    fn link_and_query_neighbor() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        let a = mem.add(NewMemory::note("A", "alpha")).unwrap();
        let b = mem.add(NewMemory::note("B", "beta")).unwrap();
        let edge_api = EdgeApi::new(&store, &cfg);
        edge_api
            .link(&a.id, &b.id, EdgeType::Related, 0.5, None, None)
            .unwrap();
        let neighbors = edge_api.neighbors(&a.id, 1).unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0.id, b.id);
    }

    #[test]
    fn idempotent_link() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        let a = mem.add(NewMemory::note("A", "alpha")).unwrap();
        let b = mem.add(NewMemory::note("B", "beta")).unwrap();
        let edge_api = EdgeApi::new(&store, &cfg);
        edge_api
            .link(&a.id, &b.id, EdgeType::Supports, 0.5, None, None)
            .unwrap();
        // Second link with higher strength must not error or duplicate.
        edge_api
            .link(&a.id, &b.id, EdgeType::Supports, 0.7, None, None)
            .unwrap();
        let neighbors = edge_api.neighbors(&a.id, 1).unwrap();
        assert_eq!(neighbors.len(), 1, "idempotent relink should not duplicate");
    }
}
