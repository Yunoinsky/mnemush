# mnemush-dsh

[DeepSeek Harness (DSH)](https://github.com/deepseek-ai/deepseek-harness) plugin for [mnemush](https://github.com/Yunoinsky/mnemush) — the brain-inspired memory layer for AI coding agents.

A native Cordis plugin (same contract as `@deepseek-ai/dsh-tool-bash`). It registers the full mnemush memory toolset on `ctx.tools`, injects the concept table (context-priming index) into the system prompt, and runs the session-end maintenance pass.

## What it does

- **16 native tools** with clean names: `memory_add`, `memory_search`, `memory_get`, `memory_link`, `memory_neighbors`, `memory_reflect`, `memory_save_search_result`, `mnemush_status`, `memory_next`, `memory_frontier`, `memory_action_create`, `memory_action_update`, `identity_propose`, `identity_list_pending`, `identity_approve`, `identity_reject`.
- **Concept-table priming** — `[memory index] N concepts` is injected into the system prompt at startup and refreshed after every memory write (and on each new session), telling the agent what's retrievable before it searches.
- **Session-end maintenance** — on `session/disposed`, runs `prune`, `edge-decay`, `process-needs-review`, and `eval prune` (gated by the same `MNEMUSH_*_ON_SESSION_END` env vars as the Pi extension; hard-delete is never auto-run).

## Install

1. Install mnemush's binaries so both `mnemush` and `mnemush-mcp` are on `PATH`:

   ```bash
   ./scripts/install.sh          # from the mnemush clone
   # or: cargo install mnemush
   ```

2. Add this package to a DSH profile. `mnemush-dsh` is not published to npm
   (yet), so install from the local clone path. The `-w` flag is required
   because a profile directory is a pnpm workspace root:

   ```bash
   dsh plugin --profile web add -w /path/to/mnemush/packages/mnemush-dsh
   ```

   The `declares no dsh.bundle — installed as a plain dependency` warning is
   expected: `mnemush-dsh` is a Cordis plugin enabled via config rows, not a
   patch bundle.

3. Enable the plugin in the profile's `cordis.patch.yml` (at
   `$DSH_HOME/profiles/web/cordis.patch.yml`, or the home-level
   `$DSH_HOME/cordis.patch.yml` to apply to every profile). **Use `insert:`** —
   a plain `id/name` row is an id-targeted patch and is skipped with
   `patch: entry not found`:

   ```yaml
   - insert:
       - id: mnemush
         name: mnemush-dsh
         config:
           conceptLimit: 40
   ```

   HMR hot-reloads the profile patch layer; editing the file applies without a
   process restart (a full `dsh web` restart is the safe fallback).

## Config

| Key | Default | Meaning |
|---|---|---|
| `binaryPath` | `""` | Absolute path to `mnemush-mcp`; empty = auto-detect. |
| `dataDir` | `""` | Custom data dir (overrides `~/.mnemush`). |
| `conceptLimit` | `40` | Max concepts injected into the system prompt. |
| `injectConceptTable` | `true` | Inject the `[memory index]` concept table. |
| `maintenanceOnSessionEnd` | `true` | Run prune/edge-decay/needs-review/eval-prune on session disposal. |

Everything else is tuned in `~/.mnemush/config.toml` (see
[docs/config.example.toml](../../docs/config.example.toml)).

## Session-end maintenance env vars

Same knobs as the Pi extension:

| Var | Default | Effect |
|---|---|---|
| `MNEMUSH_PRUNE_ON_SESSION_END` | `apply` | `apply` soft-deletes up to `MNEMUSH_PRUNE_SESSION_LIMIT` (default 5) low-confidence memories; `dry` lists only; `off` skips. |
| `MNEMUSH_EDGE_DECAY_ON_SESSION_END` | `on` | Recompute edge strength via the Ebbinghaus curve. |
| `MNEMUSH_NEEDS_REVIEW_ON_SESSION_END` | `on` | Clear stale `needs_review` flags / downgrade repeated failures. |
| `MNEMUSH_EVAL_PRUNE_ON_SESSION_END` | `on` | Apply eval-log caps. |

## Development

```bash
npm install                  # from the repo root (registers the workspace)
npm run build --workspace mnemush-dsh
npm run test --workspace mnemush-dsh
```

The plugin duck-types the DSH service surface (like `mnemush-pi` does for the
Pi SDK), so it builds without the `@deepseek-ai/*` packages installed; they are
optional peer dependencies supplied by DSH at runtime.

## License

MulanPSL-2.0
