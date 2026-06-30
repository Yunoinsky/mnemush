# mneme-opencode

OpenCode plugin for [mneme](https://github.com/Yunoinsky/mneme) — the brain-inspired memory layer for AI coding agents.

## What it does

- Auto-spawns `mneme-mcp` on first tool call (lazy connect)
- Auto-captures corrections and "remember X" from user messages
- Auto-captures tool failures as memories
- Exposes 3 native tools: `mneme-memory`, `mneme-memory-search`, `mneme-memory-link`

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
