# Mneme 🧠

> Persistent, brain-inspired memory for AI coding agents. Rust core, TS adapters for Pi and OpenCode.

**Mneme** (/ˈniːmiː/, Greek μνήμη "memory") is a local-first memory layer that gives your coding agent durable, structured, self-maintaining memory across sessions.

- **Brain-inspired design** — long-term memory graph + identity layer + tunable forgetting/reinforcement
- **Single Rust binary** — ~5–12 MB, no Python, no Docker, no cloud
- **Works for any agent** — exposes MCP (stdio) for general use, plus native hooks for Pi and OpenCode
- **Auto-maintaining** — heuristic capture (corrections, "remember X", tool errors)
- **Identity-aware** — USER/PERSONA/CONSTITUTION files inject into every session
- **Graph-structured LTM** — memories link to each other; auto-link on add
- **Tunable** — every parameter in `~/.mneme/config.toml` (half-life, prune thresholds, search weights, ...)

## Status

**v0.4.0 (2026-08-05)** — polish: backup/restore, multi-project isolation, schema-migration trait.

Backup: `mneme backup` produces a gzipped tarball of `~/.mneme/` (mneme.db via SQLite online backup API for WAL consistency, plus config.toml and identity/, optionally eval/). `mneme restore [-i FILE] [--target DIR] [--force] [--yes]` unpacks with downgrade protection.

Multi-project: opt-in via `MNEME_PROJECT=foo` — writes auto-tag, reads scope to that project. `--all-projects` on search/list or `MNEME_ALL_PROJECTS=1` bypasses. Backward-compatible: without the env, behavior matches v0.3.

Schema migration: `Migration` trait in `crates/mneme/src/migrations.rs`; registry walks in order. Each version bump is a struct impl + registry append — no `Store::migrate` changes for future bumps.

114 Rust + 31 OpenCode + 36 client + 26 hook tests as of HEAD. Install via `git clone` + `./scripts/install.sh` (publish to crates.io/npm/Homebrew deferred — see ROADMAP).

See [ROADMAP.md](ROADMAP.md) for what's done and what's next.

## Quick start

```bash
# Clone & enter
git clone https://github.com/Yunoinsky/mneme.git && cd mneme

# Install (builds Rust + TS, copies to ~/.cargo/bin, inits ~/.mneme/)
./scripts/install.sh

# Configure
$EDITOR ~/.mneme/config.toml
$EDITOR ~/.mneme/identity/USER.md

# Try the CLI
mneme add "use jose not jsonwebtoken" --category decision --importance 0.9
mneme search "jose"
mneme list
mneme stats

# v0.3: graph analytics
mneme graph pagerank -n 10        # hub detection
mneme graph communities           # community detection
mneme graph export -f dot -o graph.dot   # Graphviz

# v0.3: what am I committed to? (agent-facing; also via MCP tools)
mneme eval stats                  # self-eval log summary
```

### For Pi

```bash
# Install the extension from your local clone (not on npm yet)
pi install ./packages/mneme-pi
# restart pi
```

Pi auto-spawns `mneme-mcp` and connects via stdio on every session. Identity is auto-injected.

### For OpenCode

```bash
mkdir -p ~/.config/opencode/plugin
ln -sf "$(pwd)/packages/mneme-opencode/dist/index.js" \
       ~/.config/opencode/plugin/mneme.js
```

Restart OpenCode; the plugin will lazy-connect to `mneme-mcp` on first use.

## Architecture (one-minute version)

```
┌────────────┐  ┌────────────┐
│    Pi      │  │  OpenCode  │  (TS agents)
└─────┬──────┘  └──────┬─────┘
      │                │
   mneme-pi       mneme-opencode    (TS adapters, hooks + tools)
      │                │
      └────────┬───────┘
               │ MCP stdio
               ▼
        ┌─────────────┐
        │  mneme (Rust) │  ← single binary, 5-12 MB
        │  MCP server  │
        └──────┬──────┘
               │
               ▼
        ~/.mneme/mneme.db   (SQLite + FTS5)
```

**Three layers** in the data model:

1. **Identity** (`~/.mneme/identity/*.md`) — USER, PERSONA, CONSTITUTION. Never decays, always injected.
2. **LTM graph** (SQLite) — Procedural, Semantic, Identity nodes. Edges: related / supports / contradicts / supersedes. Decays on Ebbinghaus curve.

Three core mechanisms (brain-inspired, all in v0.1):

| Mechanism | What it does |
|---|---|
| Forgetting | Ebbinghaus-style decay with tunable half-life + importance modifier |
| Reinforcement | access_count + importance boost on every search hit |
| Active pruning | confidence + last_accessed threshold (configurable) |

## Configuration

All parameters live in `~/.mneme/config.toml`. See [docs/config.example.toml](docs/config.example.toml) for the full schema. The system works with **zero configuration** — sensible defaults are baked in.

Most-tuned parameters:

```toml
[forgetting]
half_life_days = 90.0          # how fast memories fade
prune_confidence_threshold = 0.1

[search]
default_limit = 10
weight_recency = 0.3
weight_importance = 0.2
```

## Project layout

```
mneme/
├── crates/mneme/        # Rust core (lib + 2 binaries: mneme CLI, mneme-mcp server)
├── packages/
│   ├── mneme-client/    # Shared TS client (spawns mneme-mcp, JSON-RPC, isMnemeTool)
│   ├── mneme-pi/        # Pi extension (4 hooks + 15 tools + self-eval logging)
│   └── mneme-opencode/  # OpenCode plugin (lazy connect + 16 tools + self-eval logging)
├── docs/                # ARCHITECTURE, ROADMAP, decisions (D1–D14), config example
└── scripts/             # install.sh
```

## Development

```bash
# Rust tests
cargo test --manifest-path crates/mneme/Cargo.toml

# Build everything
npm run build

# Run CLI
cargo run --bin mneme -- --db /tmp/test.db add "hello" "world"
cargo run --bin mneme -- --db /tmp/test.db search "hello"

# Run MCP server directly (for testing)
cargo run --bin mneme-mcp
```

## License

MulanPSL-2.0 — see [LICENSE](LICENSE).
