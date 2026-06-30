# Changelog

## v0.1.0 (2026-06-30) — initial release

First end-to-end working version of mneme.

### Highlights

- **Rust core** (`crates/mneme`, ~2000 lines) with single-binary
  distribution (~5–12 MB). 28 unit tests pass.
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
