# Mneme — Design Decisions

This file captures the why behind non-obvious choices. For broader context, see [ARCHITECTURE.md](../ARCHITECTURE.md) and [ROADMAP.md](../ROADMAP.md).

## D1. Why Rust + TS, not pure Rust

**Decision**: Rust core + minimal TS adapter (Pi/OpenCode), not pure Rust or pure TS.

**Why not pure Rust**: Pi and OpenCode agent runtimes are TypeScript. Their extension/plugin APIs are TS SDKs — there is no Rust SDK for either. Going pure Rust would mean implementing only the MCP surface and losing access to agent hooks (auto-capture, identity injection, periodic review triggers).

**Why not pure TS**: We want a 5–12 MB single-binary distribution, fast startup, and minimal resource footprint. A TS-only solution would either need a heavy runtime (Node 50–80 MB) or Bun (~50 MB) and forfeit the cross-agent benefit (Claude Code / Cursor are also Go/Rust, not JS).

**The split**: Rust owns storage, search, graph ops, identity, config, forgetting, MCP. TS owns the thin adapter layer: spawn Rust subprocess, speak JSON-RPC, register agent hooks, auto-capture from user/tool events.

## D2. Why SQLite + FTS5, not a vector database

**Decision**: FTS5 is the default; embedding is opt-in (planned v0.3).

**Why**: For a personal memory system with 1K–10K memories, BM25 + importance scoring + decay reaches 80%+ of the value at 0% of the embedding cost. Open benchmarks (BeIR, MS-MARCO) show BM25 remains competitive for keyword-heavy queries. Embedding models add 20–100 MB of dependencies and per-index cost.

**When to upgrade**: when query patterns shift from "what I remember" to "stuff similar in meaning to X" (semantic recall, not keyword recall). That's a v0.3 feature.

## D3. Why no episodic memory as a separate type

**Decision**: Episodic memories are `Semantic` with `category=Episodic` + a `created_at` timestamp.

**Why**: The user spec said LTM has two types (skills + knowledge). Adding a third type adds complexity (storage, queries, decay rules) without proportional value. Most "what did we do on date X" queries are served by `session_search` (full-text search across session JSONL), not by dedicated episodic memory. The category field gives us a path to upgrade later without migration.

## D4. Why simplified forgetting formula

**Decision**: Single formula `confidence = initial * 0.5^(days/half_life) * (1 + ln(access+1) * factor)`, no separate SM-2 / SRS scheduler.

**Why**: SM-2 (Anki) was designed for human flashcard study. For agent memory, the access pattern is "search hit = useful" not "scheduled review". The simplified formula captures the same behavior (use-it-or-lose-it + importance-modulated decay) in 3 lines instead of 30.

**Cost**: lose fine-grained SRS intervals. **Benefit**: one tunable knob (`half_life_days`) instead of five (stability, ease_factor, interval, due date, ...).

## D5. Why manual FTS5 sync, not triggers

**Decision**: We INSERT into `memory_fts` from Rust after each `INSERT INTO memory`, instead of via SQL triggers.

**Why**: FTS5 external-content triggers mis-parse user content containing parentheses, commas, or other special characters. The error is "fts5: syntax error near '('" and it's silent — the insert appears to succeed but the row is missing from the index. Manual sync with parameter binding avoids the issue entirely.

**Cost**: ~5 extra lines of code per write path. **Benefit**: zero surprise, content with any characters works.

## D6. Why binary UUIDs as memory rowids for FTS5

**Decision**: FTS5 rowid is INTEGER; we use the SQLite `rowid` of the `memory` table (auto-increment integer) instead of the UUID `id`.

**Why**: FTS5 `INSERT ... VALUES (?1, ...)` requires an INTEGER for `rowid`. Our `memory.id` is TEXT (UUID v7). The memory table's implicit `rowid` (INTEGER) is the natural fit — and since `id` is a PRIMARY KEY, we can always look it up.

**Cost**: one extra `last_insert_rowid()` call per insert. **Benefit**: clean FTS5 with no hash collisions.

## D7. Why a separate `Identity` layer (not just more memories)

**Decision**: USER/PERSONA/CONSTITUTION live outside the LTM graph, as separate files.

**Why**: Identity is not a memory trace — it's the architecture that shapes how memories are encoded/retrieved. Three concrete differences:
1. **Never decays** — the user's name shouldn't fade after 90 days.
2. **Always injected** — every session reads identity, not just relevant ones.
3. **Hard rules** — CONSTITUTION is human-writable only, no silent updates, no override.

**Cost**: two surfaces to maintain (markdown + graph). **Benefit**: a clean safety boundary. Identity is `read-only` from the agent's perspective.

## D8. Why a `pi` command without `/config`

**Decision**: No `/config` command in Pi. Users edit `~/.mneme/config.toml` directly.

**Why**: `/config` would conflate two concerns — agent memory operations (which the agent should handle) with system configuration (which the user owns). Mixing them invites the agent to mutate its own behavior. The Unix-philosophy alternative — "files are the API" — keeps boundaries clear and aligns with how `CLAUDE.md` / `AGENTS.md` work.

**Cost**: user must learn the file location. **Benefit**: agent can never accidentally reconfigure the system.

## D9. Why no auto-inject of memories before turns

**Decision**: Default `auto_inject_before_turn = false`. Agent must call `memory_search` to retrieve.

**Why**: Openlore's benchmark (the only one with hard data) shows auto-injection helps large/unfamiliar repos (−21% cost) but hurts small/familiar ones (+43% cost). For a personal memory system, the bias toward "small/medium/session-specific knowledge" is too high to make auto-injection net-positive. Users who want it can flip the config flag.

**Cost**: agent has to think "should I search?". **Benefit**: no token waste, agent develops better judgment about when memory is relevant.

## D10. Why no web-search auto-save

**Decision**: Web search results are NOT auto-saved to LTM. A `memory_save_search_result` tool is provided for explicit use.

**Why**: Humans don't remember every webpage they read; they remember conclusions. Auto-saving every search would bloat LTM with low-signal content. The agent can use the tool to save summaries it judges worth keeping.

## D11. Why the user spec said "no `/config`" — preserved

**Decision**: No Pi command named `/config` or anything similar. Configuration is file-based.

**Why**: The user explicitly asked for manual editing as the only way to change parameters. We respect that boundary: no in-agent tool, no in-agent command. The CLI's `mneme config` subcommand (for terminal use) is also deliberately kept minimal.

## D12. v0.1 scope discipline

**Decision**: 8 features deferred to v0.2+ (periodic LLM review, spreading activation, schema migrations, multi-project, backup, web viewer, etc.).

**Why**: v0.1 had to be a working end-to-end system that a user can install, run, and dogfood. Every deferred feature was one whose absence wouldn't break the core flow. The result is ~5,100 lines of Rust + ~1,400 lines of TS as of v0.2, instead of an estimated 15K lines of half-finished features.

**Trade-off accepted**: slower to reach feature parity with engram/pi-hermes-memory. Faster to reach a stable MVP.

## D13. Why enum parse failures surface as MnemeError, not rusqlite::Error

**Decision**: When `row_to_memory` reads a memory row whose `tier` / `category` / `source` / `memory_type` value doesn't parse to a known enum variant, the error surfaces as `MnemeError::Invalid("unknown Tier: 'active'")` and **not** the generic rusqlite wrapper `Conversion error from type Text at index: 0`.

**Why**: The original `impl From<MnemeError> for rusqlite::Error` produced `FromSqlConversionFailure(0, Type::Text, Box::new(e))`. The Display impl for that variant prints "Conversion error from type Text at index: 0" first and **only reveals the real MnemeError if you walk the source chain**. A user hit this in 2026-07: their DB had 17 rows with `tier='active'` (a value not in the v0.2 enum), and the cryptic wrapper made the failure mode impossible to debug.

**How**: The new `From` impl uses `rusqlite::Error::ToSqlConversionFailure(Box::new(e))`, whose Display shows the inner error directly. Same mechanism (so closures with `?` keep working), but the user-facing message now reads `storage error: invalid input: unknown Tier: 'active'` instead of the misleading wrapper.

**Cost**: zero — closures still propagate via `?` and the underlying MnemeError is preserved for tests / debugging.

**Migration**: the bug surfaced in the wild (17 user rows with tier='active'). Fixed in place with `UPDATE memory SET tier='global' WHERE tier='active'`; a `mneme migrate` CLI is a future addition (see v0.4 roadmap).

**Forward-compat note**: this fixes the *display* of unknown enum values but does NOT auto-accept them. A user upgrading to a future mneme with new categories still needs to be aware that any older rows may fail to read until migrated. If we ever want to silently absorb forward-incompatible values, this is the place to revisit.

## D14. Why agent self-memory is a status column on `memory`, not a separate table

**Context**: v0.3 gives the agent the ability to track its own outstanding work — commitments with a deadline, owner, and lifecycle (active → completed / abandoned). The question was: separate `task` table, or a column on `memory`?

**Decision**: a `status` column (plus `due_at`, `claimed_by`, `parent_id`, `completed_at`) on the existing `memory` table. Schema v3.

**Why**:
- A commitment is *fundamentally* a memory — it has a title, content, importance, confidence, and should decay/search like any other. A separate table would duplicate the entire memory pipeline (FTS5, decay, pruning, edges) for one extra field.
- Graph benefits: commitments link to the memories they're about via ordinary edges (`supports`), so `memory_next` can surface the task and `memory_neighbors` can show what it depends on.
- `memory_next` / `memory_frontier` filter `category != 'identity'` — identity files (USER/PERSONA/CONSTITUTION) are not actions and must never surface as commitments.

**Lifecycle ownership**: `MemoryApi::update()` owns `completed_at` — callers mutate `status`, the server sets/clears `completed_at` on terminal transitions. Callers never set it directly; the response is always re-fetched so the caller sees post-write state (a stale-response bug was fixed in 2026-08).

**Priority order** for `memory_next`: `due_at ASC` (nulls last — deadlines win), then `created_at DESC` (newest for no-deadline case), then `id DESC` (stable tie-break since SQLite timestamps can collide within a second).

**Cost**: three migration arms in `store.rs::migrate`; the v0.2→v0.3 arm adds the five columns. Half-migrated DBs (schema_version stale relative to actual columns) are handled idempotently via `pragma_table_info` checks before each `ADD COLUMN`.

**Alternatives rejected**: separate `task` table (duplicates the memory pipeline), JSON blob on `memory` (not queryable, no indexing).
