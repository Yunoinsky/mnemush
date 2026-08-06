// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Optional embedding layer (v1.0, opt-in).
//!
//! Default search uses FTS5 + BM25 + importance scoring. When the
//! `[embeddings]` config section is set, `memory_search` blends in
//! cosine similarity over a sentence-transformer embedding.
//!
//! Backends: `fastembed` (all-MiniLM-L6-v2 quantized, ~25 MB,
//! downloaded on first use to `~/.mnemush/models/`).
//!
//! Storage: SQLite table `memory_embedding(memory_id PRIMARY KEY,
//! model TEXT, dim INTEGER, vec BLOB)`. Brute-force cosine over
//! the in-memory vec set — fine for the corpus sizes mnemush targets
//! (a few thousand memories per user); switch to sqlite-vec if we
//! grow past 10k.

use std::path::PathBuf;
use std::sync::OnceLock;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::{MnemushError, Result};
use crate::store::Store;

/// Default model: all-MiniLM-L6-v2 (384-dim, ~25 MB quantized).
/// Good general-purpose English sentence embedder.
pub const DEFAULT_MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2-q";

/// Model id prefix selecting the MiniMax remote embedding backend.
/// e.g. `minimax-embo-01` → POST https://api.minimax.chat/v1/embeddings
/// with model `embo-01`. API key from `MINIMAX_API_KEY` env var.
pub const MINIMAX_PREFIX: &str = "minimax-";

/// MiniMax embedding model name (strips the `minimax-` prefix).
pub const MINIMAX_MODEL: &str = "embo-01";

/// MiniMax cn endpoint.
pub const MINIMAX_API_BASE: &str = "https://api.minimax.chat/v1";

/// Per-model `OnceLock` cache so the heavy model is loaded at most
/// once per process. `search()` uses this — first call downloads the
/// model, subsequent calls reuse the cached handle.
static CACHE: OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<Embedder>>>>,
> = OnceLock::new();

/// Get a cached embedder for `model_id`, loading on first use.
/// Returns `Arc<Mutex<Embedder>>` because fastembed's `embed` requires
/// `&mut self` and we want to share the loaded model across the
/// process (the cache lookup itself is locked).
pub fn cached_embedder(model_id: &str) -> Result<std::sync::Arc<std::sync::Mutex<Embedder>>> {
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = cache
        .lock()
        .map_err(|e| MnemushError::Other(format!("embedder cache mutex poisoned: {}", e)))?;
    if let Some(e) = guard.get(model_id) {
        return Ok(e.clone());
    }
    let emb = std::sync::Arc::new(std::sync::Mutex::new(Embedder::new(model_id)?));
    guard.insert(model_id.to_string(), emb.clone());
    Ok(emb)
}

/// Compute cosine similarity between two equal-length vectors.
/// Returns 0.0 if either vector is zero-norm (no direction).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "cosine: dimension mismatch");
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na.sqrt() * nb.sqrt()).max(f32::EPSILON);
    dot / denom
}

/// Lazily-loaded embedding model. First `embed()` call downloads
/// the model (cached in `~/.mnemush/models/` by `fastembed`) and
/// keeps it in memory for subsequent calls.
pub struct Embedder {
    /// Local fastembed backend (None when using remote).
    inner: Option<TextEmbedding>,
    /// MiniMax remote backend (None when using local).
    remote: Option<RemoteEmbedder>,
    /// Model identifier recorded alongside stored embeddings.
    model_id: String,
    /// Embedding dimensionality.
    dim: usize,
}

/// MiniMax HTTP embedding backend (`embo-01`).
struct RemoteEmbedder {
    /// API base URL, e.g. `https://api.minimax.chat/v1`.
    api_base: String,
    /// API key (from `MINIMAX_API_KEY` env).
    api_key: String,
    /// MiniMax embedding model name, e.g. `embo-01`.
    model: String,
}

impl RemoteEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = format!("{}/embeddings", self.api_base.trim_end_matches('/'));
        // Agent with explicit timeout so a hung upstream
        // request cannot freeze a bulk embed run.
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(60))
            .build();
        // MiniMax batches up to 10 texts per call.
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(10) {
            let body = serde_json::json!({
                "model": self.model,
                "type": "query",
                "texts": chunk,
            });
            let resp = agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", self.api_key))
                .set("Content-Type", "application/json")
                .send_json(body)
                .map_err(|e| MnemushError::Other(format!("minimax embed request: {}", e)))?;
            let json: serde_json::Value = resp
                .into_json()
                .map_err(|e| MnemushError::Other(format!("minimax embed response: {}", e)))?;
            let code = json["base_resp"]["status_code"].as_i64().unwrap_or(-1);
            if code != 0 {
                let msg = json["base_resp"]["status_msg"]
                    .as_str()
                    .unwrap_or("unknown");
                return Err(MnemushError::Other(format!(
                    "minimax embed API error {}: {}",
                    code, msg
                )));
            }
            let vectors = json["vectors"]
                .as_array()
                .ok_or_else(|| MnemushError::Other("minimax embed: missing vectors".into()))?;
            for v in vectors {
                let arr = v
                    .as_array()
                    .ok_or_else(|| MnemushError::Other("minimax embed: bad vector".into()))?;
                let vec: Vec<f32> = arr
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                out.push(vec);
            }
        }
        Ok(out)
    }
}

impl Embedder {
    /// Construct, downloading the model if necessary. Cached on
    /// disk so subsequent instantiations are fast.
    ///
    /// `model_id` starting with `minimax-` selects the MiniMax remote
    /// backend (API key from `MINIMAX_API_KEY`); anything else uses
    /// the local fastembed model.
    pub fn new(model_id: &str) -> Result<Self> {
        if let Some(rest) = model_id.strip_prefix(MINIMAX_PREFIX) {
            let api_key = std::env::var("MINIMAX_API_KEY")
                .map_err(|_| MnemushError::Other(
                    "minimax backend requires MINIMAX_API_KEY env var".into(),
                ))?;
            let model = if rest.is_empty() {
                MINIMAX_MODEL.to_string()
            } else {
                rest.to_string()
            };
            return Ok(Self {
                inner: None,
                remote: Some(RemoteEmbedder {
                    api_base: MINIMAX_API_BASE.to_string(),
                    api_key,
                    model,
                }),
                model_id: model_id.to_string(),
                dim: 1536, // embo-01: 1536 (verified against API response)
            });
        }
        let model = match model_id {
            m if m == DEFAULT_MODEL_ID
                || m.starts_with("sentence-transformers/all-MiniLM-L6-v2") =>
            {
                EmbeddingModel::AllMiniLML6V2Q
            }
            other => {
                return Err(MnemushError::Other(format!(
                    "unsupported embedding model: {} (only {} and minimax-* supported in this build)",
                    other, DEFAULT_MODEL_ID
                )));
            }
        };
        let cache_dir = model_cache_dir();
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| MnemushError::Other(format!("create model cache dir: {}", e)))?;
        let opts = InitOptions::new(model).with_cache_dir(cache_dir);
        let inner = TextEmbedding::try_new(opts)
            .map_err(|e| MnemushError::Other(format!("init embedding model: {}", e)))?;
        Ok(Self {
            inner: Some(inner),
            remote: None,
            model_id: model_id.to_string(),
            dim: 384, // all-MiniLM-L6-v2: 384
        })
    }

    /// Embed a batch of texts. Returns one vec per input.
    pub fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if let Some(remote) = &self.remote {
            return remote.embed(texts);
        }
        self.inner
            .as_mut()
            .ok_or_else(|| MnemushError::Other("embedder not initialized".into()))?
            .embed(texts, None)
            .map_err(|e| MnemushError::Other(format!("embed: {}", e)))
    }

    /// Embedding dimensionality. Useful for sanity checks.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Model identifier recorded alongside each stored embedding.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Default cache directory for downloaded models.
pub fn model_cache_dir() -> PathBuf {
    crate::default_data_dir().join("models")
}

/// Persisted embedding row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEmbedding {
    pub memory_id: String,
    pub model: String,
    pub dim: i64,
    /// Little-endian f32 bytes.
    pub vec: Vec<f32>,
}

/// Store / retrieve embeddings for memory ids.
///
/// The `(memory_id, model)` pair is the natural key — re-embedding
/// after a model upgrade simply writes a new row.
impl Store {
    /// Fetch embedding for a (memory_id, model) pair. Returns
    /// `None` if no embedding was ever stored.
    pub fn get_embedding(&self, memory_id: &str, model: &str) -> Result<Option<StoredEmbedding>> {
        let mut stmt = self.conn.prepare(
            "SELECT memory_id, model, dim, vec FROM memory_embedding \
             WHERE memory_id = ?1 AND model = ?2",
        )?;
        let mut rows = stmt.query_map(params![memory_id, model], |row| {
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
        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// Iterate all embeddings for `model`. Used for brute-force
    /// cosine over the full corpus. Order is unspecified.
    pub fn all_embeddings(&self, model: &str) -> Result<Vec<StoredEmbedding>> {
        let mut stmt = self
            .conn
            .prepare("SELECT memory_id, model, dim, vec FROM memory_embedding WHERE model = ?1")?;
        let rows = stmt.query_map(params![model], |row| {
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
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch embeddings for a specific set of memory ids (one model).
    /// Used by the search-blend path to avoid loading the entire
    /// `all_embeddings` set. Empty input → empty output.
    pub fn embeddings_for(
        &self,
        memory_ids: &[String],
        model: &str,
    ) -> Result<Vec<StoredEmbedding>> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = memory_ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT memory_id, model, dim, vec FROM memory_embedding \
             WHERE model = ? AND memory_id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(model.to_string())];
        for id in memory_ids {
            params_vec.push(Box::new(id.clone()));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| &**b).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
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
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// How many embeddings we have for `model` (across all memories).
    pub fn count_embeddings(&self, model: &str) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_embedding WHERE model = ?1",
            params![model],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Delete embeddings for a memory id (all models). Used when a
    /// memory is hard-deleted.
    pub fn delete_embeddings_for(&self, memory_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM memory_embedding WHERE memory_id = ?1",
            params![memory_id],
        )?;
        Ok(())
    }
}

/// Upsert one embedding. Stored as a BLOB of little-endian f32s.
/// Free function (not a method) so callers with their own
/// `Transaction` don't need `&Store`.
pub fn put_embedding_tx(
    tx: &Transaction,
    memory_id: &str,
    model: &str,
    dim: i64,
    vec: &[f32],
) -> Result<()> {
    let bytes: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
    tx.execute(
        r#"INSERT INTO memory_embedding (memory_id, model, dim, vec, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT(memory_id, model) DO UPDATE SET
               dim = excluded.dim,
               vec = excluded.vec,
               updated_at = excluded.updated_at"#,
        params![memory_id, model, dim, bytes, crate::store::Store::now_ts()],
    )?;
    Ok(())
}

/// Migrate v3 → v4: add the `memory_embedding` table for opt-in
/// semantic search. No columns are added to `memory` itself —
/// embeddings live in a separate table so the on-disk size cost is
/// zero for users who don't enable `[embeddings]`.
pub const V3_TO_V4_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS memory_embedding (
    memory_id TEXT NOT NULL,
    model TEXT NOT NULL,
    dim INTEGER NOT NULL,
    vec BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (memory_id, model)
);
CREATE INDEX IF NOT EXISTS idx_embedding_model ON memory_embedding(model);
"#;

/// Brute-force top-N search by cosine similarity. Returns memory_ids
/// sorted by descending similarity (highest first). Skips ids with
/// no embedding (and ids in `exclude`).
pub fn top_n_cosine(
    query: &[f32],
    candidates: &[(String, Vec<f32>)],
    n: usize,
    exclude: &[String],
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = candidates
        .iter()
        .filter(|(id, _)| !exclude.contains(id))
        .map(|(id, v)| (id.clone(), cosine(query, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(n);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_norm_is_zero() {
        let z = vec![0.0, 0.0, 0.0];
        let v = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine(&z, &v), 0.0);
        assert_eq!(cosine(&v, &z), 0.0);
    }

    #[test]
    fn top_n_cosine_ranks_correctly() {
        let q = vec![1.0, 0.0, 0.0];
        let cands = vec![
            ("a".into(), vec![1.0, 0.0, 0.0]),  // cosine 1.0
            ("b".into(), vec![0.9, 0.1, 0.0]),  // ~0.99
            ("c".into(), vec![0.0, 1.0, 0.0]),  // 0
            ("d".into(), vec![-1.0, 0.0, 0.0]), // -1
        ];
        let top = top_n_cosine(&q, &cands, 3, &[]);
        assert_eq!(top[0].0, "a");
        assert_eq!(top[1].0, "b");
        // c is in top 3 (cosine 0), d excluded (negative)
        assert_eq!(top[2].0, "c");
        assert!(!top.iter().any(|(id, _)| id == "d"));
    }

    #[test]
    fn top_n_cosine_respects_exclude() {
        let q = vec![1.0, 0.0];
        let cands = vec![("a".into(), vec![1.0, 0.0]), ("b".into(), vec![0.9, 0.1])];
        let top = top_n_cosine(&q, &cands, 5, &["a".into()]);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "b");
    }

    /// End-to-end: embed-ish roundtrip using fake unit vectors.
    /// We can't run a real model in unit tests (no network), so
    /// we synthesize vectors and exercise the storage path.
    #[test]
    fn store_and_retrieve_embedding_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        // No v3->v4 migration in test schema, so create the table
        // manually for this test.
        store.conn.execute_batch(V3_TO_V4_SQL).unwrap();
        // Need a parent memory row for the FK (memory_embedding has
        // no formal FK but we use memory_id as PK — no FK required).
        let tx = store.conn.unchecked_transaction().unwrap();
        put_embedding_tx(&tx, "m1", "model-x", 3, &[0.1, 0.2, 0.3]).unwrap();
        tx.commit().unwrap();
        let got = store.get_embedding("m1", "model-x").unwrap().unwrap();
        assert_eq!(got.vec, vec![0.1, 0.2, 0.3]);
        assert_eq!(got.dim, 3);
        assert!(store.get_embedding("m1", "model-y").unwrap().is_none());
        assert!(store
            .get_embedding("nonexistent", "model-x")
            .unwrap()
            .is_none());
        assert_eq!(store.count_embeddings("model-x").unwrap(), 1);
    }

    #[test]
    fn put_embedding_overwrites_on_conflict() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch(crate::store::SCHEMA_SQL).unwrap();
        store.conn.execute_batch(V3_TO_V4_SQL).unwrap();
        let tx = store.conn.unchecked_transaction().unwrap();
        put_embedding_tx(&tx, "m", "model", 2, &[1.0, 0.0]).unwrap();
        put_embedding_tx(&tx, "m", "model", 2, &[0.0, 1.0]).unwrap();
        tx.commit().unwrap();
        let got = store.get_embedding("m", "model").unwrap().unwrap();
        assert_eq!(got.vec, vec![0.0, 1.0]);
    }
}
