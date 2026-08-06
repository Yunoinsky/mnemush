# mnemush-pi

Pi extension for [mnemush](https://github.com/Yunoinsky/mnemush) — the brain-inspired memory layer for AI coding agents.

## What it does

- Spawns `mnemush-mcp` automatically on session start
- Auto-captures corrections and "remember X" patterns from user messages
- Auto-captures tool failures as memories
- Exposes 3 native Pi tools: `memory`, `memory_get`, `memory_link`

## Install

```bash
pi install npm:mnemush-pi
```

Then ensure the `mnemush-mcp` binary is on `PATH` (install via `cargo install mnemush`).

## Tools

### `memory`

Save and search memories. Two actions:

| Action | Required args | Effect |
|---|---|---|
| `add` | `title`, `content` (+ optional `category`, `importance`) | save a new memory |
| `search` | `query` (+ optional `category`, `project`, `limit`) | FTS5 search |

To update or delete a memory, fetch it with `memory_get`, then add a new one with `category=correction` and link with `edge_type=supersedes`. The MCP layer does not expose direct edit/delete in v0.1.

### `memory_get`

Fetch a single memory by its full UUID. Search hits only show an 8-char prefix; use this when you need the full id (e.g. before `memory_link`).

### `memory_link`

Create an edge between two memories (e.g. `supersedes`, `supports`, `contradicts`, `related`).

## Config

Edit `~/.mnemush/config.toml` to tune half-life, prune thresholds, search weights, etc. See [docs/config.example.toml](../../docs/config.example.toml) for the full schema.

## License

MulanPSL-2.0
