//! Configuration with 5-layer override.
//!
//! Layers (lowest to highest priority):
//! 1. Code defaults ([`Config::default`])
//! 2. `~/.mneme/config.toml` (global)
//! 3. `./.mneme.toml` (project)
//! 4. Environment variables (`MNEME_*`)
//! 5. Per-memory overrides (in `Memory` struct itself)

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{MnemeError, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub forgetting: ForgettingConfig,
    pub edges: EdgeConfig,
    pub search: SearchConfig,
    pub storage: StorageConfig,
    pub eval: EvalConfig,
    pub project: ProjectConfig,
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
            db_path: "~/.mneme/mneme.db".to_string(),
        }
    }
}

/// Bounds for the per-session self-eval NDJSON log
/// (`~/.mneme/eval/<session>.ndjson`). Three caps, applied in order:
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
/// Apply via `mneme eval prune [--apply]` or auto at session_end.
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
            // dir scannable and bounds the cost of `mneme eval stats`.
            max_session_files: 30,
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
/// `MNEME_ALL_PROJECTS=1`), reads ignore the project filter. Writes
/// are still always auto-tagged with `default_project` unless the
/// caller overrides.
///
/// Backward compatibility: with both fields at defaults, behavior
/// matches v0.3 (no project isolation). Setting `default_project` via
/// env (`MNEME_PROJECT`) or config.toml is opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub default_project: Option<String>,
    pub cross_project_search: bool,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            default_project: None,
            cross_project_search: false,
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
            let project_path = cwd.join(".mneme.toml");
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
            return Err(MnemeError::Config(
                "forgetting.half_life_days must be > 0".into(),
            ));
        }
        if self.forgetting.prune_confidence_threshold < 0.0
            || self.forgetting.prune_confidence_threshold > 1.0
        {
            return Err(MnemeError::Config(
                "prune_confidence_threshold must be in [0, 1]".into(),
            ));
        }
        if self.forgetting.prune_importance_exempt < 0.0
            || self.forgetting.prune_importance_exempt > 1.0
        {
            return Err(MnemeError::Config(
                "prune_importance_exempt must be in [0, 1]".into(),
            ));
        }
        if self.edges.max_neighbor_hops > 5 {
            return Err(MnemeError::Config("max_neighbor_hops must be <= 5".into()));
        }
        if self.search.default_limit == 0 {
            return Err(MnemeError::Config(
                "search.default_limit must be > 0".into(),
            ));
        }
        if self.eval.max_age_days < 0 {
            return Err(MnemeError::Config("eval.max_age_days must be >= 0".into()));
        }
        if self.eval.max_entries_per_file == 0 {
            return Err(MnemeError::Config(
                "eval.max_entries_per_file must be > 0".into(),
            ));
        }
        if self.eval.max_session_files == 0 {
            return Err(MnemeError::Config("eval.max_session_files must be > 0".into()));
        }
        Ok(())
    }
}

fn parse_toml(content: &str) -> Result<Config> {
    // #[serde(default)] on every Config field means: missing fields fall
    // back to defaults, so the TOML overlay alone is sufficient.
    toml::from_str(content).map_err(|e| MnemeError::Config(format!("config parse error: {}", e)))
}

fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(v) = std::env::var("MNEME_HALF_LIFE_DAYS") {
        if let Ok(d) = v.parse() {
            cfg.forgetting.half_life_days = d;
        }
    }
    if let Ok(v) = std::env::var("MNEME_DISABLE_FORGETTING") {
        if let Ok(b) = v.parse() {
            cfg.forgetting.disable_forgetting = b;
        }
    }
    // MNEME_DATA_DIR is the high-level override (also used for identity
    // files). If set and the config didn't explicitly point db_path
    // elsewhere, derive the db path from it so the CLI/MCP share the
    // same data dir across commands.
    if std::env::var("MNEME_DB_PATH").is_err() {
        if let Ok(dir) = std::env::var("MNEME_DATA_DIR") {
            cfg.storage.db_path = format!("{}/mneme.db", dir);
        }
    }
    if let Ok(v) = std::env::var("MNEME_DB_PATH") {
        cfg.storage.db_path = v;
    }
    // MNEME_PROJECT enables per-project isolation. Set to a non-empty
    // string to scope writes/reads to that project; cross-project
    // reads require MNEME_ALL_PROJECTS=1.
    if let Ok(v) = std::env::var("MNEME_PROJECT") {
        if !v.is_empty() {
            cfg.project.default_project = Some(v);
        }
    }
    if let Ok(v) = std::env::var("MNEME_ALL_PROJECTS") {
        if let Ok(b) = v.parse::<bool>() {
            cfg.project.cross_project_search = b;
        } else if v == "1" {
            cfg.project.cross_project_search = true;
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
        std::env::set_var("MNEME_HALF_LIFE_DAYS", "30.0");
        let mut c = Config::default();
        apply_env_overrides(&mut c);
        assert_eq!(c.forgetting.half_life_days, 30.0);
        std::env::remove_var("MNEME_HALF_LIFE_DAYS");
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
        // Forward-compat: configs from a newer mneme may contain sections
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
