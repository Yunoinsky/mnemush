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
pub mod capacity;
pub mod concepts;
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
pub mod neuropils;
pub mod scanner;
pub mod schema;
pub mod store;
pub mod sync;
pub mod webdav;

pub use error::{MnemushError, Result};
pub use schema::{
    Category, EdgeType, Memory, MemoryType, NewMemory, SearchHit, SearchOpts, Source, Tier,
};

/// Mnemush version, also surfaced through `mnemush --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default data directory (`$HOME/.mnemush`, with Windows fallback).
/// Windows 下当 `HOME` 未设置时回退到 `USERPROFILE`(cmd/PowerShell 直跑时
/// HOME 可能未设,USERPROFILE 恒在);再 fallback 到当前目录(legacy 行为)。
pub fn default_data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("MNEMUSH_DATA_DIR") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_else(|| ".".to_string());
    std::path::PathBuf::from(home).join(".mnemush")
}

/// Expand a leading `~/` to `$HOME` (or `%USERPROFILE%` on Windows).
/// 其他路径原样返回。
pub fn expand_tilde(s: &str) -> std::path::PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok());
        if let Some(home) = home {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(s)
}

/// Truncate to the first `n` chars, appending `…` when longer.
/// 一次遍历: 超过 n 时 stop + 加省略号.
pub fn truncate(s: &str, n: usize) -> String {
    let mut iter = s.chars();
    let taken: String = iter.by_ref().take(n).collect();
    if iter.next().is_some() {
        taken + "…"
    } else {
        taken
    }
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
