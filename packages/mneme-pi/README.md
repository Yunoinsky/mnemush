# mneme-pi

Pi extension for [mneme](https://github.com/Yunoinsky/mneme) — the brain-inspired memory layer for AI coding agents.

## What it does

- Spawns `mneme-mcp` automatically on session start
- Auto-captures corrections and "remember X" patterns from user messages
- Auto-captures tool failures as memories
- Exposes 3 native Pi tools: `memory`, `memory_search`, `memory_link`

## Install

```bash
pi install npm:mneme-pi
```

Then ensure the `mneme-mcp` binary is on `PATH` (install via `cargo install mneme`).

## Tools

### `memory`

CRUD-style memory operations. Use this to save and retrieve facts.

| Action | Required args | Effect |
|---|---|---|
| `add` | `title`, `content` (+ optional `category`, `importance`) | save a new memory |
| `search` | `query` (+ optional `limit`) | search by FTS5 |
| `remove` | `id` | (v0.1: returns an error recommending manual cleanup) |
| `replace` | `id`, `content` | (v0.1: returns an error; do remove + add) |

### `memory_search`

Same as `memory action=search` but with a stable signature and additional filters (`category`, `project`).

### `memory_link`

Create an edge between two memories (e.g. `supersedes`, `supports`, `contradicts`, `related`).

## Config

Edit `~/.mneme/config.toml` to tune half-life, prune thresholds, search weights, etc. See [docs/config.example.toml](../../docs/config.example.toml) for the full schema.

## License

MulanPSL-2.0
