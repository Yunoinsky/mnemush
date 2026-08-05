// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Graph analytics over the memory network: PageRank, label
//! propagation (communities), and DOT / D3 JSON export.
//!
//! The memory graph is: nodes = `memory` rows, edges = `memory_edge`
//! rows (directional unless `bidirectional=1`). All analytics are
//! computed in-memory over the current graph; nothing is written back
//! (scores/communities are ephemeral — persist via `mneme graph
//! export` if you want them in a file).

use std::collections::HashMap;

use crate::error::Result;
use crate::schema::{Edge, Memory};
use crate::store::Store;

/// A loaded node: memory row + its outgoing neighbors (directed edges
/// are followed one-way; bidirectional edges both ways).
#[derive(Debug)]
pub struct GraphNode {
    pub memory: Memory,
    pub out: Vec<String>,
}

/// Directed-adjacency form used by the algorithms. `out[i]` lists the
/// neighbor ids reachable from node `i`.
pub struct Graph {
    pub nodes: Vec<Memory>,
    pub index: HashMap<String, usize>,
    pub out: Vec<Vec<usize>>,
}

impl Graph {
    /// Load the full graph (non-deleted memories + non-deleted edges).
    pub fn load(store: &Store) -> Result<Graph> {
        let mems: Vec<Memory> = store
            .conn
            .prepare("SELECT * FROM memory WHERE deleted_at IS NULL ORDER BY created_at ASC")?
            .query_map([], Store::row_to_memory)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let index: HashMap<String, usize> = mems
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.clone(), i))
            .collect();
        let edges: Vec<Edge> = load_edges(store)?;
        let mut out = vec![Vec::new(); mems.len()];
        for e in &edges {
            if let (Some(&si), Some(&ti)) = (index.get(&e.source_id), index.get(&e.target_id)) {
                out[si].push(ti);
                if e.bidirectional {
                    out[ti].push(si);
                }
            }
        }
        Ok(Graph {
            nodes: mems,
            index,
            out,
        })
    }

    /// Number of reachable nodes (non-zero degree).
    pub fn active_count(&self) -> usize {
        self.out.iter().filter(|o| !o.is_empty()).count()
    }
}

/// PageRank over the memory graph. Returns per-node rank in the same
/// order as `graph.nodes`. Dangling nodes (no outgoing edges) treat
/// their mass as redistributed to all nodes (standard handling).
///
/// `damping` defaults to 0.85, `max_iter` to 100, `tol` to 1e-6.
pub fn pagerank(g: &Graph, damping: f64, max_iter: usize, tol: f64) -> Vec<f64> {
    let n = g.nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut rank = vec![1.0 / n as f64; n];
    // Per-node out-degree (dedup'd neighbors count as links once).
    let mut out_deg = vec![0usize; n];
    for (i, out) in g.out.iter().enumerate() {
        let mut seen = std::collections::HashSet::new();
        for &j in out {
            if seen.insert(j) {
                out_deg[i] += 1;
            }
        }
    }
    let base = (1.0 - damping) / n as f64;
    for _ in 0..max_iter {
        let mut next = vec![base; n];
        // Mass from dangling nodes spreads uniformly.
        let mut dangling_mass = 0.0;
        for i in 0..n {
            if out_deg[i] == 0 {
                dangling_mass += rank[i];
            }
        }
        if dangling_mass > 0.0 {
            let share = damping * dangling_mass / n as f64;
            for v in next.iter_mut() {
                *v += share;
            }
        }
        for i in 0..n {
            if out_deg[i] == 0 {
                continue;
            }
            let share = damping * rank[i] / out_deg[i] as f64;
            for &j in &g.out[i] {
                next[j] += share;
            }
        }
        let diff: f64 = rank
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        rank = next;
        if diff < tol {
            break;
        }
    }
    rank
}

/// Label propagation (community detection). Each node starts with its
/// own id as label; iteratively each node adopts the most common label
/// among its neighbors (ties broken by smallest label string, giving
/// deterministic output). Returns `label per node` (community ids are
/// node ids of the first member that won the propagation).
pub fn label_propagation(g: &Graph, max_iter: usize) -> Vec<String> {
    let n = g.nodes.len();
    let mut label: Vec<String> = g.nodes.iter().map(|m| m.id.clone()).collect();
    for _ in 0..max_iter {
        let mut changed = false;
        // Asynchronous update: use the new label of already-visited
        // neighbors within this iteration (standard LPA variant).
        for i in 0..n {
            if g.out[i].is_empty() {
                continue;
            }
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for &j in &g.out[i] {
                *counts.entry(label[j].as_str()).or_default() += 1;
            }
            // Most common; ties → smallest label (deterministic).
            let mut best: Option<&str> = None;
            let mut best_count = 0usize;
            let mut entries: Vec<(usize, &str)> = counts.iter().map(|(k, v)| (*v, *k)).collect();
            entries.sort_by(|a, b| {
                b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)) // desc count, asc label
            });
            if let Some((count, lab)) = entries.first() {
                best_count = *count;
                best = Some(lab);
            }
            if let Some(best) = best {
                if best != label[i] && best_count > 0 {
                    label[i] = best.to_string();
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    label
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a string for JSON string values.
fn esc_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// DOT export with an explicit edge list (see [`to_dot`]).
pub fn export_dot(
    g: &Graph,
    edges: &[Edge],
    ranks: Option<&[f64]>,
    communities: Option<&[String]>,
) -> String {
    let mut s = String::from("digraph mneme {\n  rankdir=LR;\n");
    if let Some(com) = communities {
        let mut palette: HashMap<String, usize> = HashMap::new();
        let mut next = 0usize;
        for c in com {
            palette.entry(c.clone()).or_insert_with(|| {
                let i = next;
                next += 1;
                i
            });
        }
        for (i, m) in g.nodes.iter().enumerate() {
            let ci = palette.get(&com[i]).copied().unwrap_or(0);
            let color = format!("#{:02x}{:02x}{:02x}", 60 + (ci * 40) % 180, 120, 220);
            s.push_str(&format!(
                "  \"{}\" [label=\"{}\", color=\"{}\", fontcolor=\"{}\"];\n",
                m.id,
                esc(&m.title),
                color,
                color
            ));
        }
    } else {
        for m in &g.nodes {
            let extra = if let Some(r) = ranks {
                let idx = g.index.get(&m.id).copied().unwrap_or(0);
                let r = r.get(idx).copied().unwrap_or(0.0);
                format!("label=\"{}\\nrank={:.4}\"", esc(&m.title), r)
            } else {
                format!("label=\"{}\"", esc(&m.title))
            };
            s.push_str(&format!("  \"{}\" [{}];\n", m.id, extra));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for e in edges {
        if seen.insert((e.source_id.clone(), e.target_id.clone())) {
            let pen = (e.strength * 3.0).max(0.5);
            let style = if e.bidirectional { "dir=both" } else { "" };
            s.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\", penwidth={:.1} {}];\n",
                e.source_id,
                e.target_id,
                esc(e.edge_type.as_str()),
                pen,
                style
            ));
        }
    }
    s.push_str("}\n");
    s
}

/// D3 JSON export with an explicit edge list (see [`to_d3_json`]).
pub fn export_d3(g: &Graph, edges: &[Edge], communities: Option<&[String]>) -> String {
    let mut palette: HashMap<String, usize> = HashMap::new();
    let mut next = 0usize;
    let mut nodes = Vec::new();
    for (i, m) in g.nodes.iter().enumerate() {
        let group = match communities {
            Some(com) => *palette.entry(com[i].clone()).or_insert_with(|| {
                let i = next;
                next += 1;
                i
            }),
            None => 0,
        };
        nodes.push(format!(
            "{{\"id\":\"{}\",\"label\":\"{}\",\"group\":{}}}",
            m.id,
            esc_json(&m.title),
            group
        ));
    }
    let mut links = Vec::new();
    for e in edges {
        links.push(format!(
            "{{\"source\":\"{}\",\"target\":\"{}\",\"value\":{:.3}}}",
            e.source_id, e.target_id, e.strength
        ));
    }
    format!(
        "{{\"nodes\":[{}],\"links\":[{}]}}",
        nodes.join(","),
        links.join(",")
    )
}

/// Load all non-deleted edges for export.
pub fn load_edges(store: &Store) -> Result<Vec<Edge>> {
    let mut stmt = store.conn.prepare(
        "SELECT id, source_id, target_id, edge_type, strength, initial_strength, \
         bidirectional, provenance, evidence, context, access_count, last_activated, \
         stability, created_at, deleted_at \
         FROM memory_edge WHERE deleted_at IS NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        let bidirectional_i: i32 = row.get(6)?;
        let access_count_i: i32 = row.get(10)?;
        let last_activated_ts: Option<i64> = row.get(11)?;
        let created_at_ts: i64 = row.get(13)?;
        let deleted_at_ts: Option<i64> = row.get(14)?;
        Ok(Edge {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            edge_type: crate::schema::EdgeType::parse(row.get::<_, String>(3)?.as_str())
                .unwrap_or(crate::schema::EdgeType::Related),
            strength: row.get(4)?,
            initial_strength: row.get(5)?,
            bidirectional: bidirectional_i != 0,
            provenance: row.get(7)?,
            evidence: row.get(8)?,
            context: row.get(9)?,
            access_count: access_count_i.max(0) as u32,
            last_activated: last_activated_ts.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
            stability: row.get(12)?,
            created_at: chrono::DateTime::from_timestamp(created_at_ts, 0)
                .unwrap_or_else(chrono::Utc::now),
            deleted_at: deleted_at_ts.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryApi;
    use crate::schema::NewMemory;

    fn setup() -> (Store, crate::config::Config) {
        (
            Store::open_in_memory().unwrap(),
            crate::config::Config::default(),
        )
    }

    fn add(api: &MemoryApi, title: &str) -> String {
        api.add(NewMemory::note(title, format!("content of {}", title)))
            .unwrap()
            .id
    }

    fn link(
        api: &crate::edge::EdgeApi,
        a: &str,
        b: &str,
        bidirectional: bool,
        edge_type: crate::schema::EdgeType,
    ) {
        api.link(a, b, edge_type, 0.5, None, None).unwrap();
        if bidirectional {
            api.link(b, a, edge_type, 0.5, None, None).unwrap();
        }
    }

    #[test]
    fn pagerank_hub_outranks_leaf() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        let hub = add(&mem, "hub");
        let l1 = add(&mem, "leaf1");
        let l2 = add(&mem, "leaf2");
        let l3 = add(&mem, "leaf3");
        let edge = crate::edge::EdgeApi::new(&store, &cfg);
        // hub → each leaf (hub is a source; leaves point back too,
        // making them "point to hub" as well — so make hub a sink hub:
        // leaves → hub only).
        link(&edge, &l1, &hub, false, crate::schema::EdgeType::Related);
        link(&edge, &l2, &hub, false, crate::schema::EdgeType::Related);
        link(&edge, &l3, &hub, false, crate::schema::EdgeType::Related);
        let g = Graph::load(&store).unwrap();
        let ranks = pagerank(&g, 0.85, 100, 1e-6);
        let h = g.index.get(&hub).copied().unwrap();
        let leaf_rank = ranks[g.index.get(&l1).copied().unwrap()];
        assert!(
            ranks[h] > leaf_rank,
            "hub ({:.4}) should outrank leaf ({:.4})",
            ranks[h],
            leaf_rank
        );
    }

    #[test]
    fn label_propagation_finds_two_communities() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        // Community A: a1-a2-a3 densely linked.
        let a1 = add(&mem, "a1");
        let a2 = add(&mem, "a2");
        let a3 = add(&mem, "a3");
        // Community B: b1-b2-b3 densely linked.
        let b1 = add(&mem, "b1");
        let b2 = add(&mem, "b2");
        let b3 = add(&mem, "b3");
        let edge = crate::edge::EdgeApi::new(&store, &cfg);
        for (x, y) in [(&a1, &a2), (&a2, &a3), (&a3, &a1)] {
            link(&edge, x, y, true, crate::schema::EdgeType::Related);
        }
        for (x, y) in [(&b1, &b2), (&b2, &b3), (&b3, &b1)] {
            link(&edge, x, y, true, crate::schema::EdgeType::Related);
        }
        // Weak bridge a3-b1.
        link(&edge, &a3, &b1, false, crate::schema::EdgeType::Related);
        let g = Graph::load(&store).unwrap();
        let labels = label_propagation(&g, 50);
        let a_labels: Vec<&String> = [&a1, &a2, &a3]
            .iter()
            .map(|id| &labels[*g.index.get(*id).unwrap()])
            .collect();
        let b_labels: Vec<&String> = [&b1, &b2, &b3]
            .iter()
            .map(|id| &labels[*g.index.get(*id).unwrap()])
            .collect();
        assert!(
            a_labels.iter().all(|l| *l == a_labels[0]),
            "community A should share one label, got {:?}",
            a_labels
        );
        assert!(
            b_labels.iter().all(|l| *l == b_labels[0]),
            "community B should share one label, got {:?}",
            b_labels
        );
        assert_ne!(
            a_labels[0], b_labels[0],
            "the two communities must not merge (got {:?} vs {:?})",
            a_labels[0], b_labels[0]
        );
    }

    #[test]
    fn export_dot_shapes() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        let a = add(&mem, "alpha");
        let b = add(&mem, "beta");
        let edge = crate::edge::EdgeApi::new(&store, &cfg);
        link(&edge, &a, &b, true, crate::schema::EdgeType::Related);
        let g = Graph::load(&store).unwrap();
        let edges = load_edges(&store).unwrap();
        let dot = export_dot(&g, &edges, None, None);
        assert!(dot.starts_with("digraph mneme {"));
        assert!(dot.contains(&format!("\"{}\"", a)));
        assert!(dot.contains(&format!("\"{}\" -> \"{}\"", a, b)));
        assert!(dot.ends_with("}\n"));
    }

    #[test]
    fn export_d3_json_shapes() {
        let (store, cfg) = setup();
        let mem = MemoryApi::new(&store, &cfg);
        let a = add(&mem, "alpha");
        let b = add(&mem, "beta");
        let edge = crate::edge::EdgeApi::new(&store, &cfg);
        link(&edge, &a, &b, true, crate::schema::EdgeType::Related);
        let g = Graph::load(&store).unwrap();
        let edges = load_edges(&store).unwrap();
        let json = export_d3(&g, &edges, None);
        assert!(json.contains("\"nodes\""));
        assert!(json.contains(&format!("\"id\":\"{}\"", a)));
        assert!(json.contains(&format!("\"source\":\"{}\"", a)));
        // Valid JSON.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
        // Bidirectional link creates two directed edges in the export.
        assert_eq!(v["links"].as_array().unwrap().len(), 2);
    }
}
