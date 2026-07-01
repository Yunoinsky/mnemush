# Changelog

## v0.3.0 (2026-08-05)

### Added

**Graph analytics over the memory network (`mneme graph`).** Completes the v0.3 graph-intelligence work.

- `mneme graph pagerank [-n N]` — PageRank hub detection. Nodes with more incoming/weighted links score higher; prints ranks descending. Standard damping 0.85, dangling-node mass redistribution.
- `mneme graph communities [--min-members N]` — community detection via label propagation. Deterministic (ties broken by smallest label).
- `mneme graph export -f dot|json [-o FILE] [--ranks] [--communities]` — Graphviz DOT or D3-force JSON export, optionally annotated with PageRank (dot: label suffix) and/or community (dot: color; json: group).

New `crates/mneme/src/graph.rs` module: in-memory graph load, PageRank, label propagation, DOT/JSON serializers. 4 unit tests (hub-outranks-leaf, two-communities, DOT shape, D3 JSON shape).

### Fixed

**Tool allowlists in the Pi extension rotted (3 places).** The insight-nudge counter missed OpenCode's v0.3 tools; the self-eval logger duplicated `memory_save_search_result`; the tool-failure capturer only skipped `mneme-`-prefixed names so Pi's unprefixed `memory*` tools leaked "tool failure" memories. Worse, the OpenCode names used underscores (`mneme-memory_search`) while OpenCode registers hyphens (`mneme-memory-search`) — none matched. Fixed with a single `isMnemeTool()` prefix-match helper (shared via mneme-client) used by all three hooks. Regression test added.

**OpenCode plugin never wrote self-eval logs**, so `mneme eval stats` only covered Pi sessions. Instrumented the tool-registration chokepoint (`registerTool` wraps every execute) so all 16 tools — including the 3 with hand-written try/catch that bypassed `tryRun` — now write `~/.mneme/eval/<session>.ndjson`. 3 new tests.

**CLI `mneme get`/`mneme list` didn't show v0.3 lifecycle fields** (status, due_at, completed_at, claimed_by, parent_id). Added.

**`call_memory_link` used `unwrap()` after `is_none()` checks** (panic risk on malformed input). Rewrote with let-else destructuring.

### Removed

- `evalArgsCache` / `setLastArgs` in mneme-pi (write-only, never read)
- `ForgettingConfig::importance_default` (defined, never consumed)

### Changed

- CLI `eval stats`/`eval dump` now use `eval::eval_dir()` (single source of truth for MNEME_DATA_DIR).
- Eval-log mtime fallback: unreadable mtime is treated as "now" instead of epoch (over-keep beats nuking live data).

### Fixed

**MCP input validation now rejects out-of-range and unknown enum values.**

`call_memory_add` and `call_memory_save_search_result` previously accepted `importance > 1` or `importance < 0` (and NaN), letting callers poison the decay formula. Now both reject out-of-range values with a clear `importance must be in [0.0, 1.0] (got X)` error. `call_memory_link` gets the same `strength` check. Helper `range_error(field, value, min, max)` in `bin/mcp.rs` is the single source of truth.

`call_memory_link` previously silently coerced unknown `edge_type` strings to `Related`. Now rejects them: `unknown edge_type: "foo" (must be one of related, supports, contradicts, supersedes)`. `call_memory_add` similarly rejects unknown `category` and `memory_type`. The argument "we don't want to lose data silently" was already the policy in v0.1 (test `unknown_category_errors_instead_of_silent_fallback`), but the implementation still wrote Note/Semantic. Now the implementation matches the policy.

`call_memory_link` previously leaked raw SQLite errors when the source or target didn't exist (`storage error: FOREIGN KEY constraint failed`). Now inserts pre-flight `SELECT 1 FROM memory` checks for both ids; missing returns `memory not found: <id>`.

`call_identity_approve` / `call_identity_reject` previously returned `null` for already-resolved proposals, leaving the caller unable to distinguish "not found" from "already resolved". New `mneme::identity::find_proposal(dir, id)` helper finds any-status proposal; the MCP layer translates `Ok(None)` into one of three messages: `proposal not found: <id>`, `proposal already approved`, `proposal already rejected`.

### Added

**`memory_neighbors` pi tool.** Was referenced in `memory_reflect`'s description ("call memory_neighbors to inspect") but not actually registered, forcing any caller to reach the graph out-of-process. Now: 10 pi tools (was 9). 1-tier BFS by default (`max_hops=2`), matching the spreading-activation config.

### Changed

**`memory` pi tool description corrected.** Said "add / search / replace / remove" but only `add` and `search` were implemented. Updated to "add or search" so the LLM doesn't expect `remove` and discover the gap at runtime.

**`after_tool_call` skip list now matches Pi tool names (not just OpenCode).** The 6/14-tool-call nudge counter previously skipped only OpenCode-style names (`mneme-memory`, `mneme-memory_search`, ...). In a pi session tool names have no `mneme-` prefix, so calling our own `memory` or `memory_get` would still increment the counter and surface the nudge — wasted reminder. Skip list now matches both prefixes.

**Pi extension file-header tool list.** Updated to enumerate all 10 tools.

13 new unit tests in `bin/mneme-mcp`: `range_error` × 4 (out-of-range, negative, NaN, boundaries); identity × 4 (unknown id, already approved, already rejected, approve-after-reject); unknown-value × 4 (unknown category, unknown memory_type, unknown source_id, unknown target_id); link-test × 1.

Total: 82 unit tests pass (69 lib + 13 bin).

## v0.2.0 — 2026-07-01

v0.2 (auto-maintenance) and v0.3 first cut (graph intelligence) squashed into a single release commit. Headline: mneme now runs without user intervention, the LLM can self-curate via new MCP tools, and search uses 1-hop graph expansion to surface related memories.

_Note: the items below were originally filed under `## Unreleased` in the prior commit, but actually shipped as part of v0.2.0. They have been moved here per `docs/RELEASING.md`. The release itself was not yet published to crates.io at the time of this audit, so the v0.2.0 cut date remains 2026-07-01._

### Fixed

**Enum parse errors surfaced as a misleading `Conversion error from type Text at index: 0` wrapper.** A user's DB had rows with `tier='active'` (not in the v0.2 `Tier` enum). The `impl From<MnemeError> for rusqlite::Error` produced `FromSqlConversionFailure(0, Type::Text, ...)` whose Display buries the real MnemeError. Changed to `ToSqlConversionFailure(Box::new(e))` — Display now shows the actual `unknown Tier: 'active'` (or similar) at the top level. Affected `row_to_memory` closure path (auto-link layer A, reflect candidates, mneme_status, MCP reads). 1 new unit test (`unknown_tier_errors_without_misleading_wrapper`); updated existing test (`unknown_category_errors_instead_of_silent_fallback`) to assert the misleading wrapper is absent. Documented as `D13` in `docs/decisions.md`.

**Active forgetting was declared but never invoked.** `should_prune` and the configured thresholds existed since v0.1 but no code path called them. Now wired through `mneme prune` (and the session_end hook) so the thresholds actually take effect.

**`sanitize_fts_query` joined tokens with a single space**, which FTS5 interprets as a phrase query (all terms in sequence) — meaning any multi-word query found nothing in practice. Changed to `OR` separator so the default semantics are "any term matches". This affected `memory_search` and the auto-link conflict detector, both of which now return more candidates (a feature, not a regression).

**`MNEME_DATA_DIR` env var was honored only by `identity::default_identity_dir` and `forget::prune_*` (via `Store::open`).** `mneme init` and the config's `db_path` ignored it, making `MNEME_DATA_DIR=/tmp/foo mneme init` write to the real `~/.mneme/identity/`. Now `init_dotfiles` and `apply_env_overrides` both honor it. `MNEME_DB_PATH` still wins if set explicitly.

**`identity approve` / `reject` required the full 36-char UUID.** Now any prefix ≥ 4 chars that matches a pending proposal works. `list-pending` output also prints the full `id:` line so users can copy-paste it.

### Added

**Edge decay.** `EdgeConfig.edge_decay_half_life_days` (default 60d) was declared in v0.1 but never applied. New `forget::current_edge_strength` and `decay_all_edges` implement the same Ebbinghaus formula used for memory confidence. Wired into the pi extension's `session_end` hook (default ON; `MNEME_EDGE_DECAY_ON_SESSION_END=off` to skip). Without this pass, the memory graph accumulated noise as edges were never weakened. New CLI: `mneme edge-decay`. 8 new unit tests.

**`process_needs_review` queue handler.** The `needs_review` flag was set by the v0.2 tool-error capture (`after_tool_call`) but never cleared. New `forget::process_needs_review(store, grace)` clears the flag on items older than the grace period and downgrades importance by 0.1 per pass on `category=failure` items (so repeated errors fade naturally). Wired into `session_end` (default ON; `MNEME_NEEDS_REVIEW_ON_SESSION_END=off` to skip; `MNEME_NEEDS_REVIEW_GRACE_DAYS=N` to adjust). New CLI: `mneme process-needs-review [--grace-days N]`. 4 new unit tests.

**Identity reflection.** The LLM can propose updates to `USER.md` / `PERSONA.md` / `CONSTITUTION.md` via the `identity_propose` MCP / CLI / pi tool, but updates are never applied silently. Proposals are written to `~/.mneme/identity/pending.jsonl` with id, target, content, reason, evidence_count, and status (`pending` | `approved` | `rejected`). The user reviews via `mneme identity list-pending` and applies with `approve` / `reject`. CLI subcommands: `mneme identity show|list-pending|propose|approve|reject`. MCP tools: `identity_propose`, `identity_list_pending`, `identity_approve`, `identity_reject`. Pi tools: `identity_propose`, `identity_review`. 8 new unit tests.

**`memory_save_search_result` tool.** Explicit (not auto) save of search hits as memories. Takes `ids` (from a prior `memory_search`) and `query` (recorded in context for provenance). Returns `{saved: [...ids], errors: [...]}` so the caller knows which inputs succeeded. Empty `ids` or missing `query` returns a proper -32602 error.

**Insight / eureka mechanism in two layers.** Layer A (algorithmic, on every `memory_add`): `auto_link_tx` step 3 runs a separate FTS5 OR-query against recent content, computes Jaccard similarity, and adds up to 3 low-strength `related` edges for memories in the `[0.05, 0.5)` similarity band. Configurable via new `edges.auto_link_weak_*` fields. Skips pairs already linked. Layer B (LLM-driven, on demand): `MemoryApi::reflect_candidates(now, since_days, limit)` returns recent least-connected memories. CLI: `mneme reflect [--since-days N] [--limit N]`. MCP: `memory_reflect`. Pi: `memory_reflect`. 6 new unit tests.

**`mneme status` subcommand + `mneme_status` MCP tool.** One-line summary of memory system state: active/soft-deleted counts, edge count, needs_review count, prune candidates (using `should_prune`), reflect candidates (last 7d), pending identity proposals.

**Spreading activation on search.** `memory_search` expands each top hit with its 1-hop neighbors, scoring them at `hit.score * edge.strength * 0.5`. Lets the LLM find related memories that didn't match the query text directly. Gated on `edges.max_neighbor_hops`; set to 0 to disable without code changes. 2 new unit tests.

**Periodic insight-save nudge (mneme-pi).** On every 6th and 14th non-mneme tool call in a turn, the pi extension surfaces a `sendStatus` reminder. Counter resets on `before_agent_start`. The existing error-capture handler skips ALL `mneme-*` tools so our own failures aren't recorded as "tool failure" memories.

**4 new config fields in `[edges]`.** `auto_link_weak_min_sim` (0.05), `auto_link_weak_max_sim` (0.5), `auto_link_weak_strength` (0.4), `auto_link_weak_limit` (3). See `docs/config.example.toml`.

### Changed

`docs/identity/PERSONA.md`: appended an agent-centric memory behavior section (preference/decision/correction capture patterns, what NOT to save). Bootstrapped automatically by `identity_propose`+`identity_approve`.

### Test count

56/56 unit tests pass.

### Known limitations (v0.2)

- Orphan FTS5 rows after hard-delete (`--isolate` removes the memory row without rebuilding `memory_fts`; a `mneme vacuum` would be a future addition).
- `mneme prune --isolate` (hard delete) is never auto-invoked; users opt in manually.

