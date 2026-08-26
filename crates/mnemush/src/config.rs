//! Configuration with 5-layer override.
//!
//! Layers (lowest to highest priority):
//! 1. Code defaults ([`Config::default`])
//! 2. `~/.mnemush/config.toml` (global)
//! 3. `./.mnemush.toml` (project)
//! 4. Environment variables (`MNEMUSH_*`)
//! 5. Per-memory overrides (in `Memory` struct itself)

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{MnemushError, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub forgetting: ForgettingConfig,
    pub edges: EdgeConfig,
    pub search: SearchConfig,
    pub storage: StorageConfig,
    pub eval: EvalConfig,
    pub embedding: EmbeddingConfig,
    pub project: ProjectConfig,
    pub capacity: CapacityConfig,
    pub sync: SyncConfig,
    pub llm: LlmConfig,
    pub dream: DreamConfig,
}

/// LLM provider configuration (v1.6.1). All entries are optional;
/// provider chain auto-detects what's reachable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Provider selection. "auto" walks the chain in order; explicit
    /// names force one provider and fail loudly if unavailable.
    pub provider: String,
    /// OpenAI-compatible base URL for the local provider (Ollama, LM Studio, llama.cpp).
    pub local_base_url: String,
    /// Local model name as served by the OpenAI-compatible endpoint.
    pub local_model: String,
    /// Optional API key for the local provider. Ollama ignores it; LM Studio
    /// accepts any non-empty string when running with auth on.
    pub local_api_key: String,
    /// Whether to fall back from the primary provider to local on any error.
    /// When false, a provider error (other than quota) is fatal.
    pub fallback_to_local: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_string(),
            local_base_url: "http://localhost:11434/v1".to_string(),
            local_model: "qwen3.8:27b-q4_K_M".to_string(),
            local_api_key: String::new(),
            fallback_to_local: true,
        }
    }
}

/// Dream scheduler / consolidation behavior (v1.6.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DreamConfig {
    /// Master switch for the scheduled dream daemon. When false, the
    /// `mnemush dream` CLI and the session-driven hook still work;
    /// only the 2am daemon is gated.
    pub enabled: bool,
    /// Daily wake-up time (24h, local timezone unless `timezone` set).
    pub scheduled_time: String,
    /// IANA timezone name for `scheduled_time`. Empty = system local.
    pub timezone: String,
    /// Token budget per day. The daemon skips dream if today's
    /// recorded LLM tokens already exceed this. Local models are
    /// "free" so we still run if local is the only path.
    pub daily_token_budget: u64,
    /// Provider for dream. `minimax` (default) pins dream to MiniMax M3;
    /// `local` / `deepseek` / `auto` pick a specific or chain-walked
    /// provider. Use the auto chain when cost is a concern; pin to
    /// MiniMax when you want stable etype discipline.
    pub provider: String,
    /// Whether to chunk candidates into smaller prompts. Off by default
    /// to preserve cross-batch link/merge/insight actions; turn on for
    /// smaller, faster prompts at the cost of those actions.
    pub chunked: bool,
    /// Candidates per chunk when `chunked = true`.
    pub chunk_size: usize,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            scheduled_time: "02:00".to_string(),
            timezone: String::new(),
            daily_token_budget: 500_000,
            provider: "minimax".to_string(),
            chunked: false,
            chunk_size: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ForgettingConfig {
    pub half_life_days: f32,
    pub half_life_importance_weight: f32,
    pub prune_confidence_threshold: f32,
    pub prune_min_confidence_for_candidate: f32,
    pub prune_max_days_no_access: i64,
    pub prune_importance_exempt: f32,
    pub access_boost_factor: f32,
    pub initial_confidence_default: f32,
    pub disable_forgetting: bool,
}

impl Default for ForgettingConfig {
    fn default() -> Self {
        Self {
            half_life_days: 90.0,
            half_life_importance_weight: 0.5,
            prune_confidence_threshold: 0.1,
            prune_min_confidence_for_candidate: 0.3,
            prune_max_days_no_access: 30,
            prune_importance_exempt: 0.7,
            access_boost_factor: 0.15,
            initial_confidence_default: 1.0,
            disable_forgetting: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeConfig {
    pub auto_link_topic_strength: f32,
    pub auto_link_supersede_min_sim: f32,
    pub auto_link_supersede_max_sim: f32,
    /// Lower bound for the weak-similarity auto-link layer. Jaccard
    /// similarity below this is treated as noise.
    pub auto_link_weak_min_sim: f32,
    /// Upper bound for the weak-similarity auto-link layer. Jaccard
    /// similarity in `[weak_min, weak_max)` is added as a low-strength
    /// `related` edge. The supersede range is `[supersede_min, supersede_max]`,
    /// so by default the weak layer covers `[0.05, 0.5)`.
    pub auto_link_weak_max_sim: f32,
    /// Strength assigned to weak-similarity auto-link edges.
    pub auto_link_weak_strength: f32,
    /// Max number of weak-similarity auto-link edges per new memory.
    pub auto_link_weak_limit: usize,
    pub edge_decay_half_life_days: f32,
    pub max_neighbor_hops: usize,
    pub auto_link_enabled: bool,
    /// Auto-merge near-duplicate snapshot-type memories (note / skill /
    /// insight / episodic). When a NEW memory's content is Jaccard-similar
    /// to an existing one at or above `auto_merge_min_sim`, the OLD one is
    /// soft-deleted and its edges retargeted to the new one. This keeps
    /// repeated captures of the same evolving document (e.g. a SKILL.md that
    /// changes slightly between sessions) from piling up as near-duplicates
    /// that exact-content-hash dedup can't catch.
    ///
    /// Distinct from supersede detection (Decision/Correction/Preference →
    /// adds a Supersedes edge, doesn't merge). Merge is destructive-ish
    /// (old memory soft-deleted, reversible) so it defaults to a stricter
    /// similarity threshold than supersede.
    pub auto_merge_enabled: bool,
    /// Minimum Jaccard similarity for auto-merge. Higher = fewer merges.
    pub auto_merge_min_sim: f32,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            auto_link_topic_strength: 0.6,
            auto_link_supersede_min_sim: 0.5,
            auto_link_supersede_max_sim: 0.95,
            auto_link_weak_min_sim: 0.05,
            auto_link_weak_max_sim: 0.5,
            auto_link_weak_strength: 0.4,
            auto_link_weak_limit: 3,
            edge_decay_half_life_days: 60.0,
            max_neighbor_hops: 2,
            auto_link_enabled: true,
            auto_merge_enabled: true,
            auto_merge_min_sim: 0.6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub default_limit: usize,
    pub weight_relevance: f32,
    pub weight_recency: f32,
    pub weight_importance: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            weight_relevance: 1.0,
            weight_recency: 0.3,
            weight_importance: 0.2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub db_path: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: "~/.mnemush/mnemush.db".to_string(),
        }
    }
}

/// Bounds for the per-session self-eval NDJSON log
/// (`~/.mnemush/eval/<session>.ndjson`). Three caps, applied in order:
///   1. Files older than `max_age_days` are removed (TTL).
///   2. Each surviving file is truncated to the most recent
///      `max_entries_per_file` lines (drop oldest).
///   3. If more than `max_session_files` files remain, the oldest are
///      removed until the cap holds (size cap).
///
/// Why all three:
///   - age alone leaves the count unbounded for heavy users
///   - count alone can keep ancient dead sessions
///   - size alone (lines per file) lets a single session dominate
///
/// Apply via `mnemush eval prune [--apply]` or auto at session_end.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvalConfig {
    pub max_age_days: i64,
    pub max_entries_per_file: usize,
    pub max_session_files: usize,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            // 30 days covers a typical monthly review cycle; users who
            // want longer history can override via [eval] in config.toml.
            max_age_days: 30,
            // 5000 lines ≈ 250 KB per file. Heavy session (lots of
            // tool calls) hits this; the cap drops the oldest entries
            // (the recent ones are the ones the user is reviewing).
            max_entries_per_file: 5000,
            // 30 files ≈ 30 distinct sessions of history. Keeps the
            // dir scannable and bounds the cost of `mnemush eval stats`.
            max_session_files: 30,
        }
    }
}

/// Opt-in semantic-search layer (v1.0).
///
/// Off by default — search uses FTS5 + BM25 + importance scoring.
/// When `enabled = true`, `memory_search` blends cosine similarity
/// over a sentence-transformer embedding with the BM25 score
/// (final = `bm25_weight * bm25 + embed_weight * cosine`).
///
/// `model`: passed to `fastembed`. Default =
/// `sentence-transformers/all-MiniLM-L6-v2-q` (quantized MiniLM,
/// 384-dim, ~25 MB, downloaded on first use to `~/.mnemush/models/`).
///
/// `bm25_weight` / `embed_weight`: blend weights. Default 0.7 / 0.3
/// favors the proven BM25 path while letting cosine break ties on
/// semantic similarity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    pub enabled: bool,
    pub model: String,
    pub bm25_weight: f32,
    pub embed_weight: f32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: crate::embeddings::DEFAULT_MODEL_ID.to_string(),
            bm25_weight: 0.7,
            embed_weight: 0.3,
        }
    }
}

/// Multi-project isolation (v0.4).
///
/// `default_project`: when set, all writes (memory_add) are tagged
/// with this project unless the caller overrides it, and all reads
/// (search, list, memory_next, memory_frontier) are filtered to this
/// project. When `None` (the default), no isolation — memories with
/// `project = NULL` are visible everywhere, and writes don't auto-tag.
///
/// `cross_project_search`: opt-in escape hatch. When true (or
/// `MNEMUSH_ALL_PROJECTS=1`), reads ignore the project filter. Writes
/// are still always auto-tagged with `default_project` unless the
/// caller overrides.
///
/// Backward compatibility: with both fields at defaults, behavior
/// matches v0.3 (no project isolation). Setting `default_project` via
/// env (`MNEMUSH_PROJECT`) or config.toml is opt-in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub default_project: Option<String>,
    pub cross_project_search: bool,
}

/// WebDAV cross-device sync (v1.5). Off by default — the transport
/// (`webdav-push` / `webdav-pull` CLI) always works when credentials
/// are set; `webdav_enabled` gates the automatic trigger (later tasks).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// Master switch for automatic WebDAV sync.
    pub webdav_enabled: bool,
    /// Debounce window (seconds) before an auto-sync fires.
    pub webdav_debounce_secs: i64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            webdav_enabled: false,
            webdav_debounce_secs: 30,
        }
    }
}

/// Capacity management (v1.3). Used by the capacity tasks: physical
/// DB size cap with eviction, neuropil cold judgment, and summary
/// entry truncation. Search hits refresh `last_accessed_at`, which
/// cold judgment reads to decide an entry has had no hits for
/// `cold_days`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityConfig {
    /// 物理上限 MB(add 时检查, 超限触发驱逐)。
    pub max_db_mb: f64,
    /// neuropil 冷判定: 入口多少天无命中 + 文件未改。
    pub cold_days: i64,
    /// 摘要入口截取字符数(规则截取 content 前 N 字符)。
    pub entry_summary_chars: usize,
    /// 驱逐每批处理条数。
    pub eviction_batch: usize,
    /// dream 采样延伸度: 每个种子点随机取 ≤m 个邻居, 延伸 2 级。
    /// 每轮覆盖 ≤10*m*m 条(5 最新 + 5 随机种子)。
    pub dream_sample_m: usize,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            max_db_mb: 100.0,
            cold_days: 30,
            entry_summary_chars: 300,
            eviction_batch: 100,
            dream_sample_m: 3,
        }
    }
}

impl Config {
    /// Load config from default locations + environment variables.
    pub fn load() -> Result<Self> {
        let mut config = Config::default();

        // L2: global config
        let global_path = crate::default_data_dir().join("config.toml");
        if global_path.exists() {
            let content = std::fs::read_to_string(&global_path)?;
            config = parse_toml(&content)?;
        }

        // L3: project config
        if let Ok(cwd) = std::env::current_dir() {
            let project_path = cwd.join(".mnemush.toml");
            if project_path.exists() {
                let content = std::fs::read_to_string(project_path)?;
                config = parse_toml(&content)?;
            }
        }

        // L4: environment overrides
        apply_env_overrides(&mut config);

        // validate
        config.validate()?;
        Ok(config)
    }

    /// Load from a specific path (used for testing).
    pub fn load_from(path: &Path) -> Result<Self> {
        let mut config = Config::default();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            config = parse_toml(&content)?;
        }
        apply_env_overrides(&mut config);
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.forgetting.half_life_days <= 0.0 {
            return Err(MnemushError::Config(
                "forgetting.half_life_days must be > 0".into(),
            ));
        }
        if self.forgetting.prune_confidence_threshold < 0.0
            || self.forgetting.prune_confidence_threshold > 1.0
        {
            return Err(MnemushError::Config(
                "prune_confidence_threshold must be in [0, 1]".into(),
            ));
        }
        if self.forgetting.prune_importance_exempt < 0.0
            || self.forgetting.prune_importance_exempt > 1.0
        {
            return Err(MnemushError::Config(
                "prune_importance_exempt must be in [0, 1]".into(),
            ));
        }
        if self.edges.max_neighbor_hops > 5 {
            return Err(MnemushError::Config(
                "max_neighbor_hops must be <= 5".into(),
            ));
        }
        if self.search.default_limit == 0 {
            return Err(MnemushError::Config(
                "search.default_limit must be > 0".into(),
            ));
        }
        if self.eval.max_age_days < 0 {
            return Err(MnemushError::Config(
                "eval.max_age_days must be >= 0".into(),
            ));
        }
        if self.eval.max_entries_per_file == 0 {
            return Err(MnemushError::Config(
                "eval.max_entries_per_file must be > 0".into(),
            ));
        }
        if self.eval.max_session_files == 0 {
            return Err(MnemushError::Config(
                "eval.max_session_files must be > 0".into(),
            ));
        }
        // ── v1.6.1 dream / llm ─────────────────────────────────────────
        // Provider name must be one of the known values (auto = chain walk).
        if let Err(e) = cfg_field_provider(&self.llm) {
            return Err(MnemushError::Config(e));
        }
        if let Err(e) = cfg_field_dream_provider(&self.dream) {
            return Err(MnemushError::Config(e));
        }
        if self.dream.chunk_size == 0 {
            return Err(MnemushError::Config(
                "dream.chunk_size must be > 0".into(),
            ));
        }
        if self.dream.scheduled_time.len() != 5
            || self.dream.scheduled_time.as_bytes().get(2) != Some(&b':')
        {
            return Err(MnemushError::Config(
                "dream.scheduled_time must be HH:MM".into(),
            ));
        }
        Ok(())
    }
}

fn cfg_field_provider(llm: &LlmConfig) -> std::result::Result<(), String> {
    match llm.provider.as_str() {
        "auto" | "minimax" | "deepseek" | "local" => Ok(()),
        other => Err(format!("llm.provider must be one of auto|minimax|deepseek|local, got: {other}")),
    }
}

fn cfg_field_dream_provider(dream: &DreamConfig) -> std::result::Result<(), String> {
    match dream.provider.as_str() {
        "auto" | "minimax" | "deepseek" | "local" => Ok(()),
        other => Err(format!("dream.provider must be one of auto|minimax|deepseek|local, got: {other}")),
    }
}

fn parse_toml(content: &str) -> Result<Config> {
    // #[serde(default)] on every Config field means: missing fields fall
    // back to defaults, so the TOML overlay alone is sufficient.
    toml::from_str(content).map_err(|e| MnemushError::Config(format!("config parse error: {}", e)))
}

fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(v) = std::env::var("MNEMUSH_HALF_LIFE_DAYS") {
        if let Ok(d) = v.parse() {
            cfg.forgetting.half_life_days = d;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DISABLE_FORGETTING") {
        if let Ok(b) = v.parse() {
            cfg.forgetting.disable_forgetting = b;
        }
    }
    // MNEMUSH_DATA_DIR is the high-level override (also used for identity
    // files). If set and the config didn't explicitly point db_path
    // elsewhere, derive the db path from it so the CLI/MCP share the
    // same data dir across commands.
    if std::env::var("MNEMUSH_DB_PATH").is_err() {
        if let Ok(dir) = std::env::var("MNEMUSH_DATA_DIR") {
            cfg.storage.db_path = format!("{}/mnemush.db", dir);
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DB_PATH") {
        cfg.storage.db_path = v;
    }
    // MNEMUSH_PROJECT enables per-project isolation. Set to a non-empty
    // string to scope writes/reads to that project; cross-project
    // reads require MNEMUSH_ALL_PROJECTS=1.
    if let Ok(v) = std::env::var("MNEMUSH_PROJECT") {
        if !v.is_empty() {
            cfg.project.default_project = Some(v);
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_ALL_PROJECTS") {
        if let Ok(b) = v.parse::<bool>() {
            cfg.project.cross_project_search = b;
        } else if v == "1" {
            cfg.project.cross_project_search = true;
        }
    }
    // ── LLM provider (v1.6.1) ─────────────────────────────────────────
    if let Ok(v) = std::env::var("MNEMUSH_LLM_PROVIDER") {
        if !v.is_empty() {
            cfg.llm.provider = v;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_LLM_BASE_URL") {
        if !v.is_empty() {
            cfg.llm.local_base_url = v;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_LLM_MODEL") {
        if !v.is_empty() {
            cfg.llm.local_model = v;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_LLM_API_KEY") {
        cfg.llm.local_api_key = v;
    }
    if let Ok(v) = std::env::var("MNEMUSH_LLM_NO_FALLBACK") {
        if let Ok(b) = v.parse::<bool>() {
            cfg.llm.fallback_to_local = !b;
        } else if v == "1" {
            cfg.llm.fallback_to_local = false;
        }
    }
    // ── Dream daemon (v1.6.1) ─────────────────────────────────────────
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_ENABLED") {
        if let Ok(b) = v.parse::<bool>() {
            cfg.dream.enabled = b;
        } else if v == "1" {
            cfg.dream.enabled = true;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_TIME") {
        if !v.is_empty() {
            cfg.dream.scheduled_time = v;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_TIMEZONE") {
        cfg.dream.timezone = v;
    }
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_TOKEN_BUDGET") {
        if let Ok(n) = v.parse::<u64>() {
            cfg.dream.daily_token_budget = n;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_PROVIDER") {
        if !v.is_empty() {
            cfg.dream.provider = v;
        }
    }
    if let Ok(v) = std::env::var("MNEMUSH_DREAM_CHUNKED") {
        if let Ok(b) = v.parse::<bool>() {
            cfg.dream.chunked = b;
        } else if v == "1" {
            cfg.dream.chunked = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_validates() {
        let c = Config::default();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn rejects_invalid_half_life() {
        let mut c = Config::default();
        c.forgetting.half_life_days = -1.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn env_overrides_half_life() {
        std::env::set_var("MNEMUSH_HALF_LIFE_DAYS", "30.0");
        let mut c = Config::default();
        apply_env_overrides(&mut c);
        assert_eq!(c.forgetting.half_life_days, 30.0);
        std::env::remove_var("MNEMUSH_HALF_LIFE_DAYS");
    }

    #[test]
    fn loads_toml() {
        let toml = r#"
            [forgetting]
            half_life_days = 30.0
            prune_confidence_threshold = 0.2
        "#;
        let c = parse_toml(toml).unwrap();
        assert_eq!(c.forgetting.half_life_days, 30.0);
        assert_eq!(c.forgetting.prune_confidence_threshold, 0.2);
    }

    #[test]
    fn tolerates_unknown_top_level_sections() {
        // Forward-compat: configs from a newer mnemush may contain sections
        // (e.g. `[identity]`, `[review]`) that this binary does not yet
        // implement. Loading must not panic; unknown sections are
        // ignored and known fields keep their values.
        let toml = r#"
            [forgetting]
            half_life_days = 30.0

            [identity]
            user_char_limit = 5000

            [review]
            session_end_batch_size = 20
        "#;
        let c = parse_toml(toml).unwrap();
        assert_eq!(c.forgetting.half_life_days, 30.0);
    }
}
