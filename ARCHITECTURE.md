# Mneme Architecture

> Brain-inspired memory layer for AI coding agents. Rust core, TS adapters.

## Design philosophy

Mneme is structured around three principles borrowed from human memory:

1. **Identity is separate from memory.** Who you are and who the agent is shapes how memories are encoded, retrieved, and evaluated — but isn't itself a memory trace. Identity is immutable, always-injected, and never decays.

2. **Memory is a graph, not a list.** Related memories strengthen each other. Patterns of use create lasting associations. The graph is the primary retrieval substrate.

3. **Forgetting is a feature, not a bug.** Memories that aren't used, reinforced, or marked important fade over time. The system explicitly prunes dead weight. This is what keeps the LTM tractable.

## Layers

```
┌─────────────────────────────────────────────────────┐
│ IDENTITY (graph-out, never decays)                  │
│  USER.md · PERSONA.md · CONSTITUTION.md            │
└────────────────────┬────────────────────────────────┘
                     │ influences
                     ▼
┌─────────────────────────────────────────────────────┐
│ LTM (graph, tunable decay)                          │
│  Procedural ─── Semantic ─── Identity-mirror        │
│       │              │                              │
│       └─ edges: related / supports /                │
│           contradicts / supersedes                  │
└────────────────────┬────────────────────────────────┘
                     │ promotion (session-end review)
                     ▼
┌─────────────────────────────────────────────────────┐
│ REVIEW QUEUE (transient)                            │
│  needs_review=true items processed by LLM           │
└─────────────────────────────────────────────────────┘
```

## Module map

### Rust core (`crates/mneme/src/`)

```
lib.rs              — public API surface + shared helpers (expand_tilde, init_tracing)
schema.rs           — Memory, Edge, MemoryType, Category, EdgeType, parse helpers
config.rs           — Config + 5-layer override (default / global / project / env / per-memory)
store.rs            — SQLite wrapper, migrations, manual FTS5 sync
memory.rs           — high-level ops: add / search / get / update / delete / list + scanner
edge.rs             — edge ops: link / neighbors (recursive CTE BFS)
forget.rs           — Ebbinghaus decay, active pruning, access boost
identity.rs         — USER/PERSONA/CONSTITUTION file load + identity sync
error.rs            — MnemeError + Result alias
bin/mcp.rs          — MCP stdio server (5 tools)
bin/cli.rs          — terminal CLI (clap)
```

### TS adapters (`packages/`)

```
mneme-client/       — shared library to spawn mneme binary + MCP RPC
mneme-pi/           — Pi extension (4 hooks + 3 tools)
mneme-opencode/     — OpenCode plugin (4 hooks + 3 tools)
```

## Data flow

### Add memory

```
client.memory_add(content, category, importance)
  ↓
TS adapter: validate, compute content_hash
  ↓
MCP: memory_add → Rust
  ↓
1. secret scanner (inline in memory.rs, no credential patterns in content)
2. dedup (content_hash collision? skip)
3. conflict (FTS5 similar entries; return candidates)
4. topic_key normalized
5. SQLite INSERT
6. FTS5 INSERT
7. auto-link:
   a. topic_key match → related edge
   b. supersede detection (hash sim 0.5–0.95, same category, older) → supersedes edge
8. return {id, conflicts: [candidates]}
```

### Search memory

```
client.memory_search(query, category, limit)
  ↓
MCP: memory_search → Rust
  ↓
1. FTS5 BM25 query (filtered by category if given)
2. for each hit, compute:
   retrievability = exp(-(now - last_accessed) / stability)
   confidence    = current_confidence(memory)
   importance_boost = 1 + importance
   score = bm25 * retrievability * confidence * importance_boost
3. (v0.3) neighbor expansion: BFS edges, top-K more
5. update last_accessed + access_count for all returned hits
6. return top-N sorted by score
```

### Session lifecycle (v0.2)

```
session_start:
  1. load identity files → inject into system prompt (frozen)
  2. process needs_review queue
  3. apply pending identity updates (if user approved)
  4. load LTM graph (lazy — actually paged on demand)

user turn:
  1. L1: heuristic capture (regex on user message)
     - "记住" / "remember" → auto-save @0.9
     - correction patterns → auto-save as Correction
  2. agent turn: agent may call memory tools as needed

after tool call:
  1. L1: heuristic capture (tool result analysis)
     - errors → auto-save as Failure @0.7
     - config file edits → auto-save as Convention @0.5
  2. (v0.2) increment turn counter; trigger periodic review at 10 turns

before_compact:
  1. emergency_save: importance > 0.7 in recent 20 turns → mark needs_review

session_end:
  1. (v0.2) full review pass on needs_review queue
  2. (v0.2) edge decay
  3. prune pass (math, fast)
  4. (v0.2) identity reflection → write to pending_identity.json
  5. flush any pending writes
```

## Storage

### SQLite schema

```sql
CREATE TABLE memory (
    id TEXT PRIMARY KEY,
    memory_type TEXT NOT NULL,  -- 'identity' | 'procedural' | 'semantic'
    tier TEXT NOT NULL,         -- 'global' | 'project' | 'skill' | 'session'
    category TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    context TEXT,
    topic_key TEXT,
    tags TEXT NOT NULL DEFAULT '',  -- space-delimited list
    project TEXT,
    source TEXT NOT NULL,

    initial_confidence REAL NOT NULL DEFAULT 1.0,
    confidence REAL NOT NULL DEFAULT 1.0,
    importance REAL NOT NULL DEFAULT 0.5,

    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,

    override_half_life REAL,
    never_prune INTEGER NOT NULL DEFAULT 0,
    never_decay INTEGER NOT NULL DEFAULT 0,

    content_hash TEXT NOT NULL,
    deleted_at INTEGER,
    needs_review INTEGER NOT NULL DEFAULT 0
);

-- Standalone FTS5 (not external-content). Synced manually from Rust
-- because FTS5 triggers mis-parse parentheses in user content.
CREATE VIRTUAL TABLE memory_fts USING fts5(
    title, content, context, tags,
    tokenize = 'unicode61 remove_diacritics 1'
);

CREATE TABLE memory_edge (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    edge_type TEXT NOT NULL,    -- 'related' | 'supports' | 'contradicts' | 'supersedes'
    strength REAL NOT NULL DEFAULT 0.5,
    initial_strength REAL NOT NULL DEFAULT 0.5,
    bidirectional INTEGER NOT NULL DEFAULT 0,
    provenance TEXT,
    evidence TEXT,
    context TEXT,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_activated INTEGER,
    stability REAL NOT NULL DEFAULT 7.0,
    created_at INTEGER NOT NULL,
    deleted_at INTEGER,
    UNIQUE(source_id, target_id, edge_type),
    FOREIGN KEY (source_id) REFERENCES memory(id) ON DELETE CASCADE,
    FOREIGN KEY (target_id) REFERENCES memory(id) ON DELETE CASCADE
);

CREATE TABLE memory_event (  -- audit log
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    memory_id TEXT,
    edge_id TEXT,
    details TEXT,
    actor TEXT,                 -- 'agent' | 'user' | 'background'
    created_at INTEGER NOT NULL
);

CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY
);
```

### File layout

```
~/.mneme/
├── identity/
│   ├── USER.md
│   ├── PERSONA.md
│   └── CONSTITUTION.md
├── config.toml
├── mneme.db
├── mneme.db-wal            (WAL journal, transient)
└── pending_identity.json   (v0.2: suggestions from identity reflection, transient)
```

## Memory types

| Type | Use | Decay | Example |
|---|---|---|---|
| **Identity** | user profile / agent persona | never | "user prefers Rust over Go" |
| **Procedural** | skills (how to do X) | normal | SKILL.md body |
| **Semantic** | facts, decisions, preferences, knowledge, episodic | normal | "auth uses jose not jsonwebtoken" |

Episodic is not a separate type — it's `Semantic` with `category=Episodic` + a timestamp.

## Edge types

| Type | Direction | Use |
|---|---|---|
| `related` | bidirectional | generic association (default) |
| `supports` | bidirectional | A provides evidence for B |
| `contradicts` | bidirectional | A and B are in conflict |
| `supersedes` | source→target | A replaces B (memory evolution) |

## Tunable parameters

See [config.example.toml](docs/config.example.toml) for the full schema. The 4 most-tuned:

| Parameter | Default | What it does |
|---|---|---|
| `forgetting.half_life_days` | 90.0 | base half-life (importance=0.5) |
| `forgetting.prune_confidence_threshold` | 0.1 | below this → prune candidate |
| `forgetting.disable_forgetting` | false | true = archive mode (never forget) |
| `search.weight_importance` | 0.2 | how much importance affects ranking |

## Performance targets

| Operation | Target (10K memories) | Target (1K) |
|---|---|---|
| Startup | < 500ms | < 100ms |
| FTS5 search | < 20ms | < 5ms |
| 2-hop neighbor (CTE) | < 50ms | < 10ms |
| Insert | < 5ms | < 2ms |
| Decay recalc (1K rows) | < 20ms | < 5ms |

## Future directions

See [ROADMAP.md](ROADMAP.md).

## Why Rust + TS, not pure Rust

Pi and OpenCode agent runtimes are TypeScript. Their extension/plugin APIs are TypeScript SDKs. There is no Rust SDK for either.

Rust is the right choice for the core: single binary, fast startup, low memory, no runtime. TS is the right choice for the adapter layer: direct access to agent hooks, native JSON-RPC via stdio.

See [DECISIONS.md](docs/DECISIONS.md) for the full rationale.
