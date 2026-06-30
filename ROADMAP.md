# Mneme Roadmap

## v0.1.0 — MVP (in progress)

Core data model, storage, basic ops. Agents can call memory tools manually.

- [x] Project skeleton (Rust workspace + TS packages)
- [x] Identity layer (USER.md, PERSONA.md, CONSTITUTION.md)
- [x] SQLite schema with FTS5
- [x] Memory types: Identity, Procedural, Semantic
- [x] Edge types: related, supports, contradicts, supersedes
- [x] Config system (5-layer override, provenance tracking, presets)
- [x] Forgetting mechanism (Ebbinghaus-style, tunable)
- [x] Reinforcement (access boost, importance)
- [x] Active pruning (threshold-based)
- [x] Auto-link on add (topic key, supersede detection)
- [x] Neighbor query (recursive CTE)
- [x] MCP server (5 core tools)
- [x] CLI (search, add, list, graph, stats, config)
- [x] TS client library (`mneme-client`)
- [x] Pi extension (4 hooks + 3 tools)
- [x] OpenCode plugin (4 hooks + 3 tools)
- [x] Documentation (README, ARCHITECTURE, config.example, identity templates)

## v0.2.0 — Auto-maintenance (next)

Heuristic capture, periodic LLM review, identity reflection.

- [ ] L1 heuristic triggers (regex on user msgs / tool calls)
  - "remember" / "记住" → auto-save @0.9
  - User corrections → auto-save as Correction
  - Tool errors → auto-save as Failure
  - Config file edits → auto-save as Convention
- [ ] L2 periodic LLM review (every 10 turns or 15 tool calls)
  - Async, in-process, never blocks the turn
  - JSON schema response
  - Fall back to subprocess on failure
- [ ] L3 session-end full review
  - Process `needs_review` queue
  - LLM-driven merge of duplicates
  - Edge decay pass
  - Identity reflection → pending suggestions (never silent)
- [ ] L4 session-start maintenance
  - Apply pending identity updates
  - Decay recalculation (lazy)
  - needs_review queue processing
- [ ] `memory_save_search_result` tool (for web findings)
- [ ] Identity reflection UI (TUI + CLI)

## v0.3.0 — Graph intelligence

- [ ] Spreading activation (BFS over edges, weighted)
- [ ] Shortest path (Dijkstra)
- [ ] PageRank for hub detection
- [ ] Label propagation for communities
- [ ] DOT export (Graphviz visualization)
- [ ] D3 JSON export (web viewer)
- [ ] `mneme graph` TUI command (tree view, navigate)

## v0.4.0 — Polish

- [ ] Backup/restore (`mneme backup`, `mneme restore`)
- [ ] Migration system (schema_version, automatic upgrades)
- [ ] Multi-project support (project-tier memories)
- [ ] Session log archival
- [ ] Performance benchmarks
- [ ] Cargo workspace release (crates.io)
- [ ] npm publish for TS packages
- [ ] Homebrew formula

## v1.0.0 — Stable

- [ ] API stability guarantees
- [ ] Backward-compatible schema migrations
- [ ] Comprehensive integration tests
- [ ] Documentation site
- [ ] User guide / tutorial
- [ ] Cross-platform CI (Linux, macOS, Windows)
- [ ] Embedded embedding model (optional ONNX)
- [ ] Cross-machine sync (Git-based like engram)

## Out of scope (likely forever)

- Cloud-hosted memory (mem0 model) — local-first only
- Multi-user collaboration (no shared dbs)
- Web-based management UI (TUI is enough)
- Vector database (BM25 is the floor, embeddings are opt-in)

## Design principles

1. **Local-first** — no API keys required, no network calls
2. **Token-aware** — never inject more than necessary
3. **Boring tech** — SQLite, Markdown, TOML, stdio
4. **Tunable** — every parameter exposed
5. **Reversible** — soft delete before hard delete, never lose data silently
6. **Documented** — every decision has a why
