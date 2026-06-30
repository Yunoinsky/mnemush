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
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub forgetting: ForgettingConfig,
    pub edges: EdgeConfig,
    pub search: SearchConfig,
    pub identity: IdentityConfig,
    pub storage: StorageConfig,
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
    pub importance_default: f32,
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
            importance_default: 0.5,
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
    pub max_edges_per_memory: usize,
    pub edge_decay_half_life_days: f32,
    pub edge_strength_floor: f32,
    pub max_neighbor_hops: usize,
    pub auto_link_enabled: bool,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            auto_link_topic_strength: 0.6,
            auto_link_supersede_min_sim: 0.5,
            auto_link_supersede_max_sim: 0.95,
            max_edges_per_memory: 50,
            edge_decay_half_life_days: 60.0,
            edge_strength_floor: 0.05,
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
    pub weight_identity_match: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            weight_relevance: 1.0,
            weight_recency: 0.3,
            weight_importance: 0.2,
            weight_identity_match: 0.2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IdentityConfig {
    pub user_char_limit: usize,
    pub persona_char_limit: usize,
    pub require_confirmation_on_update: bool,
    pub auto_update_min_evidence_count: u32,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            user_char_limit: 5000,
            persona_char_limit: 5000,
            require_confirmation_on_update: true,
            auto_update_min_evidence_count: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub db_path: String,
    pub wal_mode: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: "~/.mneme/mneme.db".to_string(),
            wal_mode: true,
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
    if let Ok(v) = std::env::var("MNEME_DB_PATH") {
        cfg.storage.db_path = v;
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
}
