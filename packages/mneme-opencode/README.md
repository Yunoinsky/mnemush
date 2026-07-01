# mneme-opencode

OpenCode plugin for [mneme](https://github.com/Yunoinsky/mneme) — the brain-inspired memory layer for AI coding agents.

## What it does

- Auto-spawns `mneme-mcp` on first tool call (lazy connect)
- Auto-captures corrections and "remember X" from user messages
- Auto-captures tool failures as memories
- On `session.created`: surfaces pending identity proposals
- On `session.deleted`: runs `mneme prune --apply`, `mneme edge-decay`, `mneme process-needs-review`
- Exposes 12 native tools (full parity with the Pi extension, prefixed with `mneme-` for OpenCode's namespace)

## Install

```bash
# Build & install
cd packages/mneme-opencode
npm install
npm run build
npm link

# Link into OpenCode's plugin directory
mkdir -p ~/.config/opencode/plugin
ln -sf "$(realpath packages/mneme-opencode/dist/index.js)" \
       ~/.config/opencode/plugin/mneme.js

# Ensure mneme-mcp is on PATH
cargo install mneme
```

## Tools

All tools return `{ content: [{ type: "text", text }], data? }` on success or `{ content: [...], isError: true }` on error.

### `mneme-memory`

```
action: "add" | "search"
- add: title + content (+ category, importance)
- search: query (+ limit)
```

### `mneme-memory-get`

```
id (full UUID)
- returns formatted memory record
```

### `mneme-memory-link`

```
source_id, target_id, edge_type (related|supports|contradicts|supersedes), strength (0.0–1.0)
```

### `mneme-memory-neighbors`

```
id, max_hops (default 2)
- returns neighbors with hop distance
```

### `mneme-memory-reflect`

```
sinceDays (default 7), limit (default 20)
- returns recent under-connected memories
```

### `mneme-memory-save-search-result`

```
ids (from prior search), query (recorded in context)
- optional category (default "note"), importance (default 0.5)
- explicit-only: no auto-save
```

### `mneme-status`

```
no args
- returns { active, soft_deleted, edges, needs_review,
            prune_candidates, reflect_candidates, pending_proposals }
```

### `identity-propose`

```
target (USER.md | PERSONA.md | CONSTITUTION.md)
content, reason
evidenceCount (default 1)
- writes to pending.jsonl; user reviews via `mneme identity list-pending`
```

### `identity-list-pending`

```
status? (pending | approved | rejected), all? (boolean)
```

### `identity-approve`

```
id (full UUID of proposal)
- appends a dated section to the target file
```

### `identity-reject`

```
id (full UUID of proposal)
- marks rejected, no file change
```

## Hooks

| OpenCode event | What we do |
|----------------|------------|
| `chat.message` | Heuristic capture: if user message contains `记住`/`记得`/etc., auto-save as a note; if `不要`/`改用`/etc., auto-save as a correction. |
| `tool.execute.after` | If a non-mneme tool returns `is_error: true`, auto-save the error as a `failure` memory with `needs_review: true` so the agent can come back and fix it. |
| `session.created` | Surface any pending identity proposals to stdout. |
| `session.deleted` | Run `mneme prune --apply`, `mneme edge-decay`, `mneme process-needs-review` to keep the graph tidy between sessions. Disable with `MNEME_*_ON_SESSION_END=off`. |

## Test

```bash
npm test   # runs test/integration.mjs — 28 cases including 12 tools and 4 hooks
```

## License

MulanPSL-2.0

## Install

```bash
# Build & install
cd packages/mneme-opencode
npm install
npm run build
npm link

# Link into OpenCode's plugin directory
mkdir -p ~/.config/opencode/plugin
ln -sf "$(realpath packages/mneme-opencode/dist/index.js)" \
       ~/.config/opencode/plugin/mneme.js

# Ensure mneme-mcp is on PATH
cargo install mneme
```

## Tools

### `mneme-memory`

```
action: "add" | "search"
- add: title + content (+ category, importance)
- search: query (+ limit)
```

### `mneme-memory-search`

```
query, category, limit
```

### `mneme-memory-link`

```
source_id, target_id, edge_type, strength
```

## License

MulanPSL-2.0
