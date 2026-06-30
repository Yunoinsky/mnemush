# Changelog

## Unreleased

### Fixed

- **`memory_add` mcp tool failed with `storage error: constraint failed`** when the user DB had FTS5 rowids that drifted away from the memory table's rowids (orphans from partial cleanup of the memory table or external SQL writes). The old code reused `memory.rowid` from `last_insert_rowid()` as the FTS5 rowid, so any orphan FTS5 row collided with the next insert. Now `insert_memory_tx` omits `rowid` from the FTS5 INSERT and lets FTS5 auto-assign — the two tables are now decoupled. `update_memory_tx` no longer touches FTS5 either: its only v0.1 caller (`search` access-boost) only mutates `confidence` / `last_accessed_at` / `access_count`, none of which FTS5 indexes.

- **Heuristic auto-capture was wired to a non-existent pi event.** mneme-pi registered a `user_prompt_submit` listener, but pi's runtime does not emit that event — the closest matches are `message_start` (carries the message object) and `before_agent_start` (carries `event.prompt` directly). Result: v0.1.0 shipped with a hook that never fired, so the heuristic-capture story was non-functional even for English keywords. Switched to `before_agent_start` and reading `event.prompt`. (mneme-opencode's `chat.message` listener is unaffected — that's a different platform with its own event names; left for the OpenCode plugin owner to verify.)

- **Silent fallback on unknown enum values when loading rows.** The `parse_*` helpers in `store.rs` (used by `row_to_memory` for the `memory_type` / `tier` / `category` / `source` columns) used to coerce unknown strings to the first enum variant (e.g. `category="decizion"` would silently become `Category::Note`). The `parse_enum!` macro now returns `Result`, `row_to_memory` propagates the error via `From<MnemeError> for rusqlite::Error`, and bogus rows fail to load with `MnemeError::Invalid("unknown Category: 'decizion'")`. New test `unknown_category_errors_instead_of_silent_fallback` locks the behavior. The same pattern in `bin/cli.rs::Add` (`Category::parse(&category).unwrap_or(Category::Note)`) is still present on the write path and is left as a follow-up.

- **Heuristic capture silently failed for Chinese keywords.** `looksLikeRemember` and `looksLikeCorrection` in `mneme-client` used `\b...\b` boundaries, but JavaScript's `\b` only matches between ASCII word characters — every CJK keyword (`记住`, `备忘`, `不要`, `错了`, ...) silently failed because Chinese characters are not `\w`. After this fix: substring match (no `\b`), keyword lists expanded to 10 entries each (added 记得 / 重要 / note that / key point for remember; 应该是 / 改用 / never use / use X not Y for correction), and a 36-case test file (mneme-client/test/regex.test.ts) locks the behavior. The same fix unlocks true auto-capture for Chinese users.

### Added

- **TS unit tests** for `mneme-client` (zero-deps: Node's built-in `--test` + `--experimental-strip-types`). Run via `npm test` at the package or workspace root; runs in CI alongside the Rust tests.

### Removed (dead code)

- `MnemeError::Constitutional` variant — never constructed.
- `Store::insert_edge_tx` and `Store::row_to_edge` — never called (edges are written via inline SQL in `EdgeApi`).
- `Store::parse_edge_type` (and `EdgeType` import in `store.rs`) — only used by the deleted `row_to_edge`.
- mneme-pi `memory` tool's `replace` and `remove` actions — both always returned an error suggesting the same workaround. Use `memory_get` to fetch, then add a new memory with `category=correction` and link with `edge_type=supersedes`.
- mneme-pi `memory_search` tool — a thin wrapper around `memory action=search`. Search filters (`category`, `project`, `limit`) moved to `memory action=search`, so the wrapper was redundant.
- mneme-opencode's `formatSearchHit as formatHit` rename — pointless alias; also dropped the unused `formatMemory` import.

### Refactored

- Secret/PII patterns moved from `memory.rs` into a new `scanner` module. The TODO comment about a future `scanner.rs` file is gone because the file now exists.
- Five `parse_*` functions in `store.rs` consolidated behind a `parse_enum!` macro. All enum variants must be listed in the macro; missing entries now surface as test failures (e.g. forgetting `Tier::Global` immediately breaks `round_trip_memory_row`).
- `Store::migrate()` simplified — the v1→v2 no-op arm is gone. New DBs get `INSERT INTO schema_version`; future binaries still refuse newer schemas.

## v0.1.0 (2026-06-30) — initial release

First end-to-end working version of mneme.

### Highlights

- **Rust core** (`crates/mneme`, ~1800 lines) with single-binary
  distribution (~5–12 MB). 27 unit tests pass.
- **MCP server** (`mneme-mcp`) over JSON-RPC 2.0 stdio. 5 tools:
  `memory_add`, `memory_search`, `memory_get`, `memory_link`,
  `memory_neighbors`.
- **CLI** (`mneme`) with `search`, `add`, `get`, `list`, `delete`,
  `stats`, `identity`, `config`, `init` subcommands. End-to-end
  smoke test (`scripts/test-mcp.py`) validates the full MCP flow.
- **TS client** (`packages/mneme-client`) spawns `mneme-mcp` and
  speaks JSON-RPC. Shared by both agent adapters.
- **Pi extension** (`packages/mneme-pi`) with 4 hooks (session_start,
  session_end, user_prompt_submit, after_tool_call) and 3 tools
  (`memory`, `memory_search`, `memory_link`). Heuristic auto-capture
  of remember/correction patterns and tool failures.
- **OpenCode plugin** (`packages/mneme-opencode`) with lazy-connect
  and 3 tools.
- **Config system** with 5-layer override (defaults / global /
  project / env / per-memory), env-var fallbacks, validation.
- **Identity layer** — USER.md, PERSONA.md, CONSTITUTION.md loaded
  from `~/.mneme/identity/` and rendered as a system-prompt block.
- **Ebbinghaus-style forgetting** with importance-modulated half-life,
  access-count boost, active pruning thresholds, Identity exempt.
- **Graph LTM** with 4 edge types (related, supports, contradicts,
  supersedes), BFS neighbor query (recursive CTE), auto-link on
  add (topic match + supersede detection), idempotent link
  (max-strength on duplicate).
- **Secret scanner** blocks known credential patterns at write time.

### Installation

```bash
./scripts/install.sh   # builds, copies to ~/.cargo/bin, inits ~/.mneme
```

### Test status

- 28/28 unit tests pass (`cargo test --manifest-path crates/mneme/Cargo.toml`)
- MCP smoke test passes (`python3 scripts/test-mcp.py`)
- TypeScript builds clean (`npx tsc` in each package)
- CLI end-to-end: add / list / search / stats all functional

### Known limitations (v0.1)

- MCP only exposes add/search/get/link/neighbors; replace/remove not
  yet wired through MCP (tool returns a clear error suggesting the
  workaround).
- No periodic LLM review yet — planned for v0.2.
- No spreading activation over graph yet — planned for v0.3.
- No multi-project scope filter yet (memories are global only).
- No migration system for upgrading existing v0.1 dbs to future
  versions; the schema_version field is wired but unused.
