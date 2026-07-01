# Mneme Roadmap

## v0.1.0 — Released 2026-06-30 ✅

Core data model, storage, MCP server, CLI, Pi + OpenCode adapters. See [CHANGELOG.md](CHANGELOG.md).

## v0.2.0 — Released 2026-07-01 ✅

Auto-maintenance + v0.3 first cut (graph intelligence) shipped in the same release.

- Periodic LLM review — batch-process `needs_review` queue, merge duplicates, decay edges. *(partly done: `reflect_candidates` surfaces candidates; full batch processor not built)*
- Identity reflection — LLM-suggested updates to USER/PERSONA, never applied silently. **DONE** — `identity_propose` / `identity_list_pending` / `identity_approve` / `identity_reject` via MCP + CLI + pi; `pending.jsonl` + atomic approve/reject; session_start surfaces pending.
- `memory_save_search_result` tool (explicit, not auto-save). **DONE** — MCP + TS + pi; `{saved, errors}` response shape.
- Session-start/session-end maintenance passes. **DONE** — `mneme prune --apply` runs at session_end (default ON, reversible soft-delete); `mneme_status` shows system state; session_start surfaces pending identity proposals and reflect-candidate count via `sendStatus`.

## v0.3.0 — Graph intelligence + agent self-memory 🟡 In progress (first cut shipped)

- Agent self-memory: the agent tracks its own commitments (status active/completed/abandoned, due_at, claimed_by, parent_id). **DONE** — `memory_action_create` / `memory_action_update` / `memory_next` / `memory_frontier` MCP tools + TS + Pi + OpenCode; `completed_at` auto-managed by the server. See [D14](docs/decisions.md#d14-why-agent-self-memory-is-a-status-column-on-memory-not-a-separate-table).
- Spreading activation over edges (weighted BFS). **DONE (first cut)** — `memory_search` now expands each top hit with 1-hop neighbors, score = hit.score * edge.strength * 0.5; gated on `edges.max_neighbor_hops`.
- PageRank hub detection, label propagation for communities. **DONE** — `mneme graph pagerank` / `mneme graph communities` (CLI). In-memory over the current graph; deterministic LPA tie-breaking.
- DOT/D3 export (Graphviz, web viewer). **DONE** — `mneme graph export -f dot|json` with `--ranks` / `--communities` annotations.
- Self-eval observability. **DONE** — `mneme eval stats|dump|prune`; per-session NDJSON written by both Pi and OpenCode; bounded log (30d TTL / 5000 lines/file / 30 files).

## v0.4.0 — Polish 📋 Planned

Each bullet ends with **Done when:** so progress is unambiguous.

- Backup/restore (`mneme backup`, `mneme restore`). **Done when:** CLI commands produce/restore a tarball of `~/.mneme/` and round-trip identically through a clean install.
- Schema migration system (automatic upgrades). **Done when:** every schema change ships with a `Migration` trait impl; a v0.3 db upgrades to v0.4 without manual SQL.
- Multi-project support. **Done when:** `--project <name>` flag isolates memories per project; default project = `default`; cross-project search is opt-in.
- Publish to crates.io, npm, Homebrew. **Done when:** `cargo install mneme`, `npm i -g mneme-pi`, `brew install mneme` all work end-to-end.

## v1.0.0 — Stable 📋 Planned

- API stability, comprehensive integration tests, docs site. **Done when:** every public API surface is `pub`-documented, integration tests cover each tool end-to-end, and a static docs site is published.
- Cross-platform CI (Linux, macOS, Windows). **Done when:** CI green on all three for `cargo test`, `cargo build --release`, and `npm run build`.
- Optional embedding model (ONNX) for semantic recall. **Done when:** `mneme search --embed` returns cosine-similar neighbors when embeddings are enabled; off by default (see D2).
- Cross-machine sync (Git-based). **Done when:** a fresh clone of the sync repo + `mneme init` restores state identically; conflicts surface for manual resolution.

## Out of scope

- Cloud hosting — local-first only.
- Multi-user collaboration.
- Web-based UI — TUI is enough.
- Vector DB as default (see [decisions.md D2](docs/decisions.md#d2-why-sqlite--fts5-not-a-vector-database) for the why; embeddings are opt-in, planned v1.0).
