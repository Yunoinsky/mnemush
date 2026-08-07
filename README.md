# Mnemush 🧠

> Persistent, brain-inspired memory for AI coding agents. Rust core, TS adapters for Pi and OpenCode.

**Mnemush** is a portmanteau of **mneme** (Greek μνήμη, "memory") and **mushroom** — a nod to the insect **mushroom body** (蕈体 / 蘑菇体), the brain structure responsible for learning and memory in insects (flies, bees, ants). Just as the mushroom body stores sparse, distributed, associative memories that let an insect generalize across contexts, Mnemush keeps your agent's memories as a linked graph that auto-consolidates — distributed associative storage, with the "fruiting body" being the retrievable memory that emerges from the network when it matters.

- **Brain-inspired design** — long-term memory graph + identity layer + tunable forgetting/reinforcement
- **Single Rust binary** — ~5–12 MB, no Python, no Docker, no cloud
- **Works for any agent** — exposes MCP (stdio) for general use, plus native hooks for Pi and OpenCode
- **Auto-maintaining** — heuristic capture (corrections, "remember X", tool errors)
- **Identity-aware** — USER/PERSONA/CONSTITUTION files inject into every session
- **Graph-structured LTM** — memories link to each other; auto-link on add
- **Tunable** — every parameter in `~/.mnemush/config.toml` (half-life, prune thresholds, search weights, ...)

## Status

**v1.2.0 (2026-08-07)** — LLM-driven consolidation + active forgetting.

- `mnemush consolidate [--dry-run|--suggest] [--project] [--since]` — incremental
  integration + active forgetting: LLM (MiniMax M3, DeepSeek fallback) reads new
  memories and emits actions — update / link / merge / insight (顿悟) / decay /
  forget — guarded by protection rules (importance ≥ 0.7, never_prune, identity,
  < 7 days). Incremental position in `~/.mnemush/consolidate.json`.
- `mnemush dream [--dry-run|--suggest]` — full-corpus pass with stronger
  forgetting (sleep-replay analogy); for cron. Does not advance the incremental
  position.
- Forgetting leaves a `forget_trace` meta-memory (forgetting itself is
  information): searchable, analyzable, itself forgettable later, no
  trace-of-trace recursion.
- v1.1: `mnemush import-tree <dir> --project <name>` / `export-tree` — any
  markdown file tree is a neuropil; wikilinks become mushroom-body graph edges.
- v1.0.1: semantic recall — vector top-K merges into search candidates (Chinese ↔
  English zero-overlap queries now surface).
- Earlier: v0.4 backup/restore + multi-project isolation + schema-migration
  trait; v0.3 graph analytics + eval.

151 Rust tests green at HEAD (133 lib + 18 bin). Install via
`git clone` + `./scripts/install.sh` (publish to crates.io/npm/Homebrew
deferred — see ROADMAP).

## Quick start

```bash
# Clone & enter
git clone https://github.com/Yunoinsky/mnemush.git && cd mnemush

# Install (builds Rust + TS, copies to ~/.cargo/bin, inits ~/.mnemush/)
./scripts/install.sh

# Configure
$EDITOR ~/.mnemush/config.toml
$EDITOR ~/.mnemush/identity/USER.md

# Try the CLI
mnemush add "use jose not jsonwebtoken" --category decision --importance 0.9
mnemush search "jose"
mnemush list
mnemush stats

# v0.3: graph analytics
mnemush graph pagerank -n 10        # hub detection
mnemush graph communities           # community detection
mnemush graph export -f dot -o graph.dot   # Graphviz

# v0.3: what am I committed to? (agent-facing; also via MCP tools)
mnemush eval stats                  # self-eval log summary

# v1.2: LLM-driven consolidation + active forgetting
mnemush consolidate --dry-run       # preview what the LLM would do
mnemush consolidate                 # integrate new memories + forget stale ones
mnemush dream                       # nightly full-corpus pass (stronger forgetting)
```

### For Pi

```bash
# Install the extension from your local clone (not on npm yet)
pi install ./packages/mnemush-pi
# restart pi
```

Pi auto-spawns `mnemush-mcp` and connects via stdio on every session. Identity is auto-injected.

### For OpenCode

```bash
mkdir -p ~/.config/opencode/plugin
ln -sf "$(pwd)/packages/mnemush-opencode/dist/index.js" \
       ~/.config/opencode/plugin/mnemush.js
```

Restart OpenCode; the plugin will lazy-connect to `mnemush-mcp` on first use.

## Architecture (one-minute version)

```
┌────────────┐  ┌────────────┐
│    Pi      │  │  OpenCode  │  (TS agents)
└─────┬──────┘  └──────┬─────┘
      │                │
   mnemush-pi       mnemush-opencode    (TS adapters, hooks + tools)
      │                │
      └────────┬───────┘
               │ MCP stdio
               ▼
        ┌─────────────┐
        │  mnemush (Rust) │  ← single binary, 5-12 MB
        │  MCP server  │
        └──────┬──────┘
               │
               ▼
        ~/.mnemush/mnemush.db   (SQLite + FTS5)
```

**Three layers** in the data model:

1. **Identity** (`~/.mnemush/identity/*.md`) — USER, PERSONA, CONSTITUTION. Never decays, always injected.
2. **LTM graph** (SQLite) — Procedural, Semantic, Identity nodes. Edges: related / supports / contradicts / supersedes. Decays on Ebbinghaus curve.

Three core mechanisms (brain-inspired, all in v0.1):

| Mechanism | What it does |
|---|---|
| Forgetting | Ebbinghaus-style decay with tunable half-life + importance modifier |
| Reinforcement | access_count + importance boost on every search hit |
| Active pruning | confidence + last_accessed threshold (configurable) |

## Configuration

All parameters live in `~/.mnemush/config.toml`. See [docs/config.example.toml](docs/config.example.toml) for the full schema. The system works with **zero configuration** — sensible defaults are baked in.

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
mnemush/
├── crates/mnemush/        # Rust core (lib + 2 binaries: mnemush CLI, mnemush-mcp server)
├── packages/
│   ├── mnemush-client/    # Shared TS client (spawns mnemush-mcp, JSON-RPC, isMnemushTool)
│   ├── mnemush-pi/        # Pi extension (4 hooks + 15 tools + self-eval logging)
│   └── mnemush-opencode/  # OpenCode plugin (lazy connect + 16 tools + self-eval logging)
├── docs/                # ARCHITECTURE, ROADMAP, decisions (D1–D14), config example
└── scripts/             # install.sh
```

## Development

```bash
# Rust tests
cargo test --manifest-path crates/mnemush/Cargo.toml

# Build everything
npm run build

# Run CLI
cargo run --bin mnemush -- --db /tmp/test.db add "hello" "world"
cargo run --bin mnemush -- --db /tmp/test.db search "hello"

# Run MCP server directly (for testing)
cargo run --bin mnemush-mcp
```

## Documentation (v1.0)

The project ships docs at three levels:

1. **Rust API** — auto-published to [docs.rs/mnemush](https://docs.rs/mnemush) on every crates.io release (driven by `Cargo.toml` metadata: description, license, repository, keywords, categories). Generate locally with `cargo doc --manifest-path crates/mnemush/Cargo.toml --no-deps --open`.
2. **TypeScript API** — generated by [typedoc](https://typedoc.org/) via `npm run docs:ts` → `target/docs/typedoc/index.html`. Config in `typedoc.json`.
3. **Conceptual docs** — markdown in this repo, served directly by GitHub: [README](README.md) · [ARCHITECTURE](ARCHITECTURE.md) · [ROADMAP](ROADMAP.md) · [CHANGELOG](CHANGELOG.md) · [decisions](docs/decisions.md) · [config example](docs/config.example.toml) · [release process](docs/RELEASING.md).

API stability for v1.0: all public items carry `///` doc comments (enforced via `#![warn(missing_docs)]` in `lib.rs`); new pub items without docs trigger CI warnings. `cargo doc` builds clean.

## License

MulanPSL-2.0 — see [LICENSE](LICENSE).
