// Copyright (c) 2026 Yunoinsky Chen
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! Mnemush — brain-inspired memory layer for AI coding agents.
//!
//! This is the Rust core. The library exposes:
//! - High-level memory operations ([`memory`])
//! - Graph operations over edges ([`edge`])
//! - Forgetting / reinforcement / pruning ([`forget`])
//! - Identity file loading ([`identity`])
//! - Configuration with 5-layer override ([`config`])
//!
//! Binaries (`mnemush` CLI, `mnemush-mcp` server) are in `bin/`.

pub mod backup;
pub mod config;
pub mod consolidate;
pub mod edge;
pub mod embeddings;
pub mod error;
pub mod eval;
pub mod forget;
pub mod graph;
pub mod identity;
pub mod llm;
pub mod memory;
pub mod migrations;
pub mod scanner;
pub mod schema;
pub mod store;
pub mod sync;
pub mod neuropils;

pub use error::{MnemushError, Result};
pub use schema::{
    Category, EdgeType, Memory, MemoryType, NewMemory, SearchHit, SearchOpts, Source, Tier,
};

/// Mnemush version, also surfaced through `mnemush --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default data directory (`$HOME/.mnemush`).
pub fn default_data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("MNEMUSH_DATA_DIR") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mnemush")
}

/// Expand a leading `~/` to `$HOME`. Other paths are returned unchanged.
pub fn expand_tilde(s: &str) -> std::path::PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(s)
}

/// Initialize tracing subscriber. Honors `RUST_LOG`; defaults to `warn`.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let _ = fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .try_init();
}
