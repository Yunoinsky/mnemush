# Mnemush Roadmap

## v0.1.0 — Released 2026-06-30 ✅

Core data model, storage, MCP server, CLI, Pi + OpenCode adapters. See [CHANGELOG.md](CHANGELOG.md).

## v0.2.0 — Released 2026-07-01 ✅

Auto-maintenance + v0.3 first cut (graph intelligence) shipped in the same release.

- Periodic LLM review — batch-process `needs_review` queue, merge duplicates, decay edges. *(partly done: `reflect_candidates` surfaces candidates; full batch processor not built)*
- Identity reflection — LLM-suggested updates to USER/PERSONA, never applied silently. **DONE** — `identity_propose` / `identity_list_pending` / `identity_approve` / `identity_reject` via MCP + CLI + pi; `pending.jsonl` + atomic approve/reject; session_start surfaces pending.
- `memory_save_search_result` tool (explicit, not auto-save). **DONE** — MCP + TS + pi; `{saved, errors}` response shape.
- Session-start/session-end maintenance passes. **DONE** — `mnemush prune --apply` runs at session_end (default ON, reversible soft-delete); `mnemush_status` shows system state; session_start surfaces pending identity proposals and reflect-candidate count via `sendStatus`.

## v0.3.0 — Graph intelligence + agent self-memory 🟡 In progress (first cut shipped)

- Agent self-memory: the agent tracks its own commitments (status active/completed/abandoned, due_at, claimed_by, parent_id). **DONE** — `memory_action_create` / `memory_action_update` / `memory_next` / `memory_frontier` MCP tools + TS + Pi + OpenCode; `completed_at` auto-managed by the server. See [D14](docs/decisions.md#d14-why-agent-self-memory-is-a-status-column-on-memory-not-a-separate-table).
- Spreading activation over edges (weighted BFS). **DONE (first cut)** — `memory_search` now expands each top hit with 1-hop neighbors, score = hit.score * edge.strength * 0.5; gated on `edges.max_neighbor_hops`.
- PageRank hub detection, label propagation for communities. **DONE** — `mnemush graph pagerank` / `mnemush graph communities` (CLI). In-memory over the current graph; deterministic LPA tie-breaking.
- DOT/D3 export (Graphviz, web viewer). **DONE** — `mnemush graph export -f dot|json` with `--ranks` / `--communities` annotations.
- Self-eval observability. **DONE** — `mnemush eval stats|dump|prune`; per-session NDJSON written by both Pi and OpenCode; bounded log (30d TTL / 5000 lines/file / 30 files).

## v0.4.0 — Released 2026-08-05 ✅

Polish: backup/restore, schema-migration trait, multi-project isolation. See [CHANGELOG.md](CHANGELOG.md).

## v0.4.0 — Polish ✅ Shipped

Each bullet ends with **Done when:** so progress is unambiguous.

- Backup/restore (`mnemush backup`, `mnemush restore`). **DONE** — gzipped tar archive containing `mnemush.db` (via SQLite online backup API for WAL consistency), `config.toml`, `identity/`, optionally `eval/`. Archive starts with `MANIFEST.json` carrying version, schema_version, counts. Restore refuses to overwrite a target with a newer schema_version (downgrade guard); pass `--force` to override. Prompts for confirmation by default.
- Schema migration system (automatic upgrades). **DONE** — `Migration` trait in `crates/mnemush/src/migrations.rs`; each version bump is a `Migration` impl in the registry. Adding a new version: write a struct impl'ing `Migration`, append to `default_registry()`. Migrations are idempotent (pragma_table_info guards) so re-running on a half-migrated DB is safe. `Store::migrate` walks the registry in order and bumps `schema_version` after each.
- Multi-project support. **DONE** — opt-in via `MNEMUSH_PROJECT=foo` (auto-tag writes, scope reads). `--all-projects` on `search`/`list` bypasses isolation; `MNEMUSH_ALL_PROJECTS=1` makes it the default. Without MNEMUSH_PROJECT, behavior is identical to v0.3 (NULL projects visible everywhere) — fully backward-compatible. `SearchOpts.cross_project_override` is the CLI-only escape hatch (TS client never sets it).
- Publish to crates.io, npm, Homebrew. **DEFERRED** — the repo is the source of truth; users install via `git clone` + `./scripts/install.sh` for now. When ready: `cargo install mnemush`, `npm i -g mnemush-pi`, `brew install mnemush` should all work end-to-end. Pre-publish checklist: Cargo.toml license/description/repo, `cargo package` dry-run, npm package.json fields, brew formula.

## v1.0.0 — Stable ✅ Released 2026-08-06

- API stability, comprehensive integration tests, docs site. **DONE** (partial — see ⚠️ below) — `#![warn(missing_docs)]` enforces /// on public items; `cargo doc` builds clean (5 pre-existing HTML lint warnings, no broken links). Three docs levels: Rust API → [docs.rs/mnemush](https://docs.rs/mnemush) (auto-publish on crates.io release); TS API → typedoc via `npm run docs:ts`; conceptual → GitHub-rendered markdown. Integration tests: 96 lib + 18 bin Rust + 36 client + 31 OpenCode + 26 hook = 207 tests.
  ⚠️ **NOT FULLY DONE**: 185 mostly-field-level missing_docs warnings remain (the lib has `#[allow(missing_docs_on_fields)]`-equivalent semantics because we use `warn`, not `deny`). Doc build passes; users see full API on docs.rs. Filling in remaining field-level docs is mechanical polish — defer to future patches.
- Cross-platform CI (Linux, macOS, Windows). **DONE** — `windows-latest` added to all three jobs (rust, ts, mcp-smoke). All `run:` steps use `shell: bash` so Linux/macOS/Windows get identical semantics. mcp-smoke gets `setup-python@v5` so the smoke script finds `python3`. Strategy uses `fail-fast: false` on the windows matrix so a single-OS failure doesn't hide the others. (Windows runners pre-installed fastembed's ONNX Runtime native bindings; rusqlite's bundled SQLite is cross-platform.)
- Optional embedding model (ONNX) for semantic recall. **DONE** — `fastembed` crate wired up; `memory_embedding` table (schema v3 → v4 migration); `mnemush embed [--title-contains PAT] [--force]` CLI for backfill; config block `[embedding] enabled = false` by default. `MemoryApi::search` blends cosine similarity with BM25 when embeddings are enabled (final score = `bm25_weight * norm(bm25) + embed_weight * cosine`); per-process model cache via `OnceLock<Mutex<HashMap>>` so the heavy model loads once. Falls back silently to BM25 if the embedder can't load or no embeddings are stored yet. 2 new tests in `memory.rs` (disabled-branch works, cosine math).
- Auto-merge near-duplicate memories. **DONE** — when a new note/skill/insight/episodic memory is Jaccard-similar (>= `auto_merge_min_sim`, default 0.6) to an existing one, the old one is soft-deleted and its edges retargeted to the new. Catches evolving-document re-captures that exact content-hash dedup misses (the 2026-08-06 "10x using-superpowers" incident). Decision/Correction/Preference keep supersede edges. Config: `[edges] auto_merge_enabled` / `auto_merge_min_sim`. 3 unit tests.
- Cross-machine sync (Git-based). **DONE** — `mnemush sync init|export|import` (Git as transport, mnemush as codec). Sync dir layout: `MANIFEST.json` (schema_version + counts), `memory.json` (all rows), `edges.json`, `identity/` (verbatim USER/PERSONA/CONSTITUTION + pending.jsonl), `embeddings/` (one JSON per (model,memory_id)). `init` runs `git init` + commits; user does `git remote add`/`push`/`pull` themselves. `import` refuses snapshots from newer schema_version; reports per-memory conflicts (local updated_at newer) but leaves those rows for manual resolution. Verified: fresh export of the live home DB (138 memories) round-trips. Fresh-DB migration bug fixed along the way (schema_version table accumulated rows via INSERT OR REPLACE; now INSERT-then-UPDATE keeps a single row).

## v1.1.0 — Neuropils 文件树记忆 ✅ Released 2026-08-07

- 任意目录树即记忆: `mnemush import-tree <dir> --project <name>` / `export-tree`。frontmatter 解析、wikilink → 边、增量同步。文件 = 权威源, mnemush 维护关系。
- 架构命名: 内容层 = **neuropils**, 索引层 = **mushroom_body**。

## v1.2.0 — LLM 驱动巩固 + 主动遗忘 ✅ Released 2026-08-07

- `mnemush consolidate`: 增量收集 → LLM(MiniMax M3 / DeepSeek fallback)→ 6 类动作 update/link/merge/insight/decay/forget。保护规则(importance≥0.7/never_prune/identity/7天)。
- `mnemush dream`: 每日全量巩固 + 遗忘高峰。**遗忘痕迹**(forget_trace): 忘掉什么本身也是信息, 可再遗忘, 防 trace-of-trace。
- 主动遗忘生物映射: 双阈值(低 confidence 易忘, 高 confidence 需强证据)。

## v1.3.0 — 容量管理 + neuropil 归档 ✅ Released 2026-08-07

- 100MB 物理硬阈值 + 驱逐链(清可再生 wiki 索引 → 低分软删 → VACUUM)。
- **neuropil 化**: 可结构化记忆 export 到文件树, 主库留摘要入口(语义可命中)。
- **neuropil 压缩**: 30 天双条件冷判定 → 合并归档页 + tar.gz 打包。
- dream 三合一: 遗忘 + neuropilize 复核 + 冷压缩 + 容量报告。**dream 采样滚动覆盖**(5 最新 + 5 随机种子 + 2 级图延伸 ≤90 条/轮)。

## v1.4.0 — 概念表(context priming index)✅ Released 2026-08-07

- `mnemush concepts`: importance×recency×access top-N + title 压缩。
- Pi 插件 session_start 注入 + 写入时刷新 —— agent 知道记忆里有什么可搜(前额叶检索线索类比)。

## Out of scope

- Cloud hosting — local-first only.
- Multi-user collaboration.
- Web-based UI — TUI is enough.
- Vector DB as default (see [decisions.md D2](docs/decisions.md#d2-why-sqlite--fts5-not-a-vector-database) for the why; embeddings are opt-in, planned v1.0).
