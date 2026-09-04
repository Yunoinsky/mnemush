# Mnemush 🧠

> Persistent, brain-inspired memory for AI coding agents. Rust core, TS adapters for Pi, OpenCode, and DeepSeek Harness (DSH).

**Mnemush** is a portmanteau of **mneme** (Greek μνήμη, "memory") and **mushroom** — a nod to the insect **mushroom body** (蕈体 / 蘑菇体), the brain structure responsible for learning and memory in insects (flies, bees, ants). Just as the mushroom body stores sparse, distributed, associative memories that let an insect generalize across contexts, Mnemush keeps your agent's memories as a linked graph that auto-consolidates — distributed associative storage, with the "fruiting body" being the retrievable memory that emerges from the network when it matters.

## Architecture in one minute

One index hub, many content trees — mirroring insect neurobiology:

```
             ┌──────────────────────────────────────┐
             │ MUSHROOM_BODY (index layer, SQLite)  │
             │  agent experience: full text +       │
             │  vectors + graph; summary entries    │
             │  (title+path); cross-cluster edges   │
             └───┬──────────────┬──────────────┬────┘
                 │ import-tree  │ import-tree  │ ... (flexible, N trees)
                 ▼              ▼              ▼
        ┌──────────────┐ ┌──────────────┐   ┌──────────────┐
        │ NEUROPIL A   │ │ NEUROPIL B   │   │ NEUROPIL …   │
        │ file tree    │ │ file tree    │   │ file tree    │
        │ = memory     │ │ = memory     │   │ = memory     │
        └──────────────┘ └──────────────┘   └──────────────┘
                 ▲              ▲              ▲
                 └────── export-tree / neuropilize (摘要入口回写) ──────┘
```

- **mushroom_body** — one DB (SQLite + FTS5 + vectors) at the top. It's the single index/association hub: every neuropil's content is indexed here incrementally (`mnemush import-tree <dir> --project <name>`), and cross-cluster edges live here regardless of which neuropil the nodes came from. Retrieval, consolidation, and forgetting happen here.
- **neuropils** — any number of independent directory trees below it, each a file-system-managed memory source (concepts, papers, knowledge bases, …). Files are the authoritative source, directly readable via grep/cat/tree and Git-versionable. The `…` denotes an arbitrary N — add/remove a neuropil by importing/cleaning its tree without touching the others.

```
        ┌────────┐  ┌──────────┐  ┌─────────┐
        │   Pi   │  │ OpenCode │  │   DSH   │   (TS agents)
        └───┬────┘  └────┬─────┘  └────┬────┘
            │ mnemush-pi / mnemush-opencode / mnemush-dsh (hooks + tools)
            └─────────────┬────────────┘
                          │ MCP stdio
                          ▼
                 ┌────────────────┐
                 │ mnemush (Rust) │   ← single binary: MCP server + CLI
                 └────────┬───────┘
                          ▼
        ~/.mnemush/mnemush.db   (mushroom_body)
        ~/.mnemush/neuropils/   (default neuropil dir)
```

**Brain mapping**: neuropils = cortex (content stored in place); mushroom_body = hippocampus/mushroom body (index + associations); consolidate = memory consolidation; dream = sleep consolidation + forgetting peak; concept table = prefrontal retrieval cues; forget_trace = forgetting trace (forgetting itself is information).

## Status

**v1.6.0 (2026-08-21)** — WebDAV cross-device sync, automatic: memory writes (add/update/soft-delete) mark dirty, a background push fires 30s after the last write (debounced); disabled by default (`[sync] webdav_enabled=false`, enable via `MNEMUSH_WEBDAV_USER` / `MNEMUSH_WEBDAV_PASS`). Two-way merge (newer wins + union + deletion propagation) with ETag optimistic locking.

**v1.5.0 (2026-08-14)** — DeepSeek Harness plugin: `mnemush-dsh` native Cordis plugin, 16 memory tools + concept-table injection + session maintenance (same contract as Pi plugin).

**v1.4.0 (2026-08-07)** — concept table (context priming index): `mnemush concepts` ranks top-N by importance×recency×access; Pi extension injects at session_start and refreshes on writes. Tells the agent what's retrievable (prefrontal retrieval cues).

**v1.3.0 (2026-08-07)** — capacity management: 100MB physical cap + eviction chain + neuropilization + cold archive, folded into the nightly dream pass.

**v1.2.0 (2026-08-07)** — LLM-driven consolidation + active forgetting: `mnemush consolidate` / `mnemush dream` (forgetting + forget traces).

**v1.1.0 (2026-08-07)** — neuropils file-tree memory: `mnemush import-tree` / `export-tree`; any directory tree is memory.

**v1.0.0 (2026-08-06)** — stable: API stability, cross-platform CI, semantic recall, auto-merge, Git sync.

**v0.4 (2026-08-05)** — backup/restore, multi-project isolation, schema migrations. **v0.3** — graph analytics + self-eval. **v0.1-0.2** — core storage + MCP + auto-maintenance.

See [CHANGELOG.md](CHANGELOG.md) and [ROADMAP.md](ROADMAP.md).

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
mnemush status                    # incl. capacity line: DB size/cap + neuropil entry count

# v1.1: neuropils — any directory tree is memory
mnemush import-tree ~/my-knowledge --project wiki   # index a file tree (frontmatter + wikilink)
mnemush export-tree ~/out --project wiki            # export back to a file tree

# v1.2: LLM-driven consolidation + active forgetting
mnemush consolidate --dry-run     # preview what the LLM would do
mnemush consolidate               # incremental: update/link/merge/insight/decay/forget
mnemush dream                     # nightly: consolidation + neuropilize + cold archive + capacity report

# v1.4: concept table (priming index)
mnemush concepts --limit 40       # top-N concepts, injected into agent context
```

## For Pi / OpenCode / DSH

```bash
# Pi extension (from your local clone)
npm install -g /path/to/mnemush/packages/mnemush-pi
# or symlink into ~/.pi/agent/extensions/
# restart pi: session_start injects the concept table, memory tools available, auto-capture on
```

- Pi extension injects `[memory index] N concepts` at session_start and refreshes on memory writes
- Heuristic capture: corrections, "remember X", tool errors auto-imported
- `mnemush-worker` agent can use the full memory toolset standalone

```bash
# DeepSeek Harness (DSH) plugin — native Cordis plugin (install from the local clone)
dsh plugin --profile web add -w /path/to/mnemush/packages/mnemush-dsh
# then enable it in the profile's cordis.patch.yml (must use `insert:`):
#   - insert:
#       - id: mnemush
#         name: mnemush-dsh
#         config: { conceptLimit: 40 }
```

- DSH plugin registers all 16 memory tools (`memory_add`, `memory_search`, …, `identity_reject`)
- Injects the `[memory index]` concept table into the system prompt and refreshes it on writes
- Runs the same session-end maintenance (prune / edge-decay / needs-review / eval-prune) on `session/disposed`

See [packages/mnemush-dsh/README.md](packages/mnemush-dsh/README.md).

## Features

- **One hub, many trees** — a single mushroom_body (index/graph) flexibly indexes any number of file-system neuropils (content)
- **Semantic recall** — MiniMax embo-01 vectors blended with FTS5 (zero-overlap CN↔EN queries hit)
- **Graph LTM** — memories interlink, auto-link on add; PageRank/communities
- **LLM consolidation + active forgetting** — consolidate/dream, dual-threshold + protection (importance≥0.7/never_prune/identity/7d)
- **Capacity self-governance** — 100MB cap eviction chain + neuropilization + cold archive
- **Concept-table priming** — persistent memory index in agent context
- **Identity layer** — USER/PERSONA/CONSTITUTION injected every session
- **Single binary** — ~5-12 MB, no Python/Docker/cloud

## Configuration

Everything is tunable in `~/.mnemush/config.toml` (see [docs/config.example.toml](docs/config.example.toml)):

- `[forgetting]` — half-life, prune thresholds, access boost
- `[capacity]` — `max_db_mb` (physical cap), `cold_days` (cold judgment), `dream_sample_m` (dream sampling fan-out)
- `[embedding]` — semantic recall toggle + MiniMax model
- `[project]` — multi-project isolation (MNEMUSH_PROJECT)
- `[edges]` — auto-link / auto-merge thresholds

## Project layout

```
crates/mnemush/        — Rust core(binary + lib)
  src/neuropils.rs     — file-tree import/export (content layer)
  src/consolidate.rs   — LLM consolidation + dream engine
  src/capacity.rs      — capacity eviction / summary entries / cold archive
  src/concepts.rs      — concept table ranking + title compression
  src/llm.rs           — MiniMax/DeepSeek chat client
  src/memory.rs        — add/search/get/update + semantic recall
  src/embeddings.rs    — MiniMax embo-01 vectors
  src/edge.rs          — graph edges + BFS neighbors
  src/forget.rs        — forgetting curve + prune + forget traces
packages/mnemush-pi/   — Pi extension (concept-table injection + memory tools)
packages/mnemush-opencode/ — OpenCode plugin
packages/mnemush-dsh/  — DeepSeek Harness (DSH) plugin (memory tools + concept priming + maintenance)
packages/mnemush-client/   — shared TS client
docs/                  — architecture / decisions / config example / superpowers design archive
```

## Development

```bash
# Rust tests
cargo test --manifest-path crates/mnemush/Cargo.toml

# Build everything
npm run build --workspaces

# Run CLI
cargo run --manifest-path crates/mnemush/Cargo.toml -- search "jose"

# Run MCP server directly (for testing)
cargo run --manifest-path crates/mnemush/Cargo.toml --bin mnemush-mcp
```

180 Rust tests green at HEAD (162 lib + 18 bin), plus TS tests across all four packages (client 39 / opencode 31 integration).

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — architecture & modules
- [docs/decisions.md](docs/decisions.md) — design decision record
- [docs/config.example.toml](docs/config.example.toml) — config reference
- [docs/superpowers/](docs/superpowers/) — design archive (specs + plans)

## License

[MulanPSL-2.0](LICENSE)
