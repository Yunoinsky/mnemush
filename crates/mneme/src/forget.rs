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

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::schema::Memory;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Category, MemoryType, Source, Tier};
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
}
