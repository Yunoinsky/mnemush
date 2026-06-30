# Mneme Roadmap

## v0.1.0 — Done

Core data model, storage, MCP server, CLI, Pi + OpenCode adapters. See [CHANGELOG.md](CHANGELOG.md).

## v0.2.0 — Auto-maintenance (next)

- Periodic LLM review — batch-process `needs_review` queue, merge duplicates, decay edges.
- Identity reflection — LLM-suggested updates to USER/PERSONA, never applied silently.
- `memory_save_search_result` tool (explicit, not auto-save).
- Session-start/session-end maintenance passes.

## v0.3.0 — Graph intelligence

- Spreading activation over edges (weighted BFS).
- PageRank for hub detection, label propagation for communities.
- DOT/D3 export (Graphviz, web viewer).

## v0.4.0 — Polish

- Backup/restore (`mneme backup`, `mneme restore`).
- Schema migration system (automatic upgrades).
- Multi-project support.
- Publish to crates.io, npm, Homebrew.

## v1.0.0 — Stable

- API stability, comprehensive integration tests, docs site.
- Cross-platform CI (Linux, macOS, Windows).
- Optional embedding model (ONNX) for semantic recall.
- Cross-machine sync (Git-based).

## Out of scope

- Cloud hosting — local-first only.
- Multi-user collaboration.
- Web-based UI — TUI is enough.
- Vector DB as default — BM25 is the floor, embeddings opt-in.
