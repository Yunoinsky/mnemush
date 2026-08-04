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

## v0.4.0 — Released 2026-08-05 ✅

Polish: backup/restore, schema-migration trait, multi-project isolation. See [CHANGELOG.md](CHANGELOG.md).

## v0.4.0 — Polish ✅ Shipped

Each bullet ends with **Done when:** so progress is unambiguous.

- Backup/restore (`mneme backup`, `mneme restore`). **DONE** — gzipped tar archive containing `mneme.db` (via SQLite online backup API for WAL consistency), `config.toml`, `identity/`, optionally `eval/`. Archive starts with `MANIFEST.json` carrying version, schema_version, counts. Restore refuses to overwrite a target with a newer schema_version (downgrade guard); pass `--force` to override. Prompts for confirmation by default.
- Schema migration system (automatic upgrades). **DONE** — `Migration` trait in `crates/mneme/src/migrations.rs`; each version bump is a `Migration` impl in the registry. Adding a new version: write a struct impl'ing `Migration`, append to `default_registry()`. Migrations are idempotent (pragma_table_info guards) so re-running on a half-migrated DB is safe. `Store::migrate` walks the registry in order and bumps `schema_version` after each.
- Multi-project support. **DONE** — opt-in via `MNEME_PROJECT=foo` (auto-tag writes, scope reads). `--all-projects` on `search`/`list` bypasses isolation; `MNEME_ALL_PROJECTS=1` makes it the default. Without MNEME_PROJECT, behavior is identical to v0.3 (NULL projects visible everywhere) — fully backward-compatible. `SearchOpts.cross_project_override` is the CLI-only escape hatch (TS client never sets it).
- Publish to crates.io, npm, Homebrew. **DEFERRED** — the repo is the source of truth; users install via `git clone` + `./scripts/install.sh` for now. When ready: `cargo install mneme`, `npm i -g mneme-pi`, `brew install mneme` should all work end-to-end. Pre-publish checklist: Cargo.toml license/description/repo, `cargo package` dry-run, npm package.json fields, brew formula.

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
