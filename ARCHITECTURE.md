# Mnemush Architecture

> Brain-inspired memory layer for AI coding agents. Rust core, TS adapters.

## Design philosophy

Mnemush is structured around three principles borrowed from human memory:

1. **Identity is separate from memory.** Who you are and who the agent is shapes how memories are encoded, retrieved, and evaluated — but isn't itself a memory trace. Identity is immutable, always-injected, and never decays.

2. **Memory is a graph, not a list.** Related memories strengthen each other. Patterns of use create lasting associations. The graph is the primary retrieval substrate.

3. **Forgetting is a feature, not a bug.** Memories that aren't used, reinforced, or marked important fade over time. The system explicitly prunes dead weight. This is what keeps the LTM tractable.

## Layers

Three layers, mirroring insect neurobiology (v1.1+):

```
┌─────────────────────────────────────────────────────┐
│ IDENTITY (never decays, always injected)            │
│  USER.md · PERSONA.md · CONSTITUTION.md            │
└────────────────────┬────────────────────────────────┘
                     │ influences
                     ▼
┌─────────────────────────────────────────────────────┐
│ NEUROPILS (内容层, 文件树)                           │
│  任意 markdown 目录树 = 记忆权威源                    │
│  grep/cat/tree 直接读, Git 版本化                    │
│  import-tree/export-tree 双向同步                    │
└────────────────────┬────────────────────────────────┘
         按需加载(import)│ neuropil 化(export + 摘要入口)
                     ▼
┌─────────────────────────────────────────────────────┐
│ MUSHROOM_BODY (索引层, 主库 SQLite)                  │
│  agent 经验: 全文 + 向量 + 边(常驻)                   │
│  摘要入口: neuropil 化记忆留 title+摘要+路径           │
│  wiki 动态索引: 按需局部加载(可再清)                  │
│  跨簇关联边(related/supports/contradicts/supersedes) │
│  巩固: consolidate/dream · 遗忘: decay/forget        │
│  容量: 100MB 硬阈值驱逐链 + 冷归档                    │
└─────────────────────────────────────────────────────┘
```

**大脑映射**:neuropils = 皮层(内容就地存储),mushroom_body = 海马/蘑菇体(索引 + 关联)。文件树与主库是同一记忆的两层:内容在文件树,索引在 mushroom_body,边跨两层维护。

## Module map

### Rust core (`crates/mnemush/src/`)

```
lib.rs              — public API surface + shared helpers (expand_tilde, init_tracing)
schema.rs           — Memory, Edge, MemoryType, Category, EdgeType, parse helpers
config.rs           — Config + 5-layer override; capacity/embedding/project 各段
store.rs            — SQLite wrapper, migrations, manual FTS5 sync
memory.rs           — high-level ops: add / search / get / update / delete / list
scanner.rs          — secret / PII pattern detection (called by add path)
edge.rs             — edge ops: link / neighbors (recursive CTE BFS)
graph.rs            — PageRank, label propagation, DOT / D3-JSON export (v0.3)
forget.rs           — Ebbinghaus decay, active pruning, access boost, 遗忘痕迹
identity.rs         — USER/PERSONA/CONSTITUTION file load + identity sync
eval.rs             — self-eval NDJSON log maintenance (TTL / line / file caps)
error.rs            — MnemushError + Result alias
neuropils.rs        — 内容层: 文件树导入/导出, frontmatter + wikilink (v1.1)
consolidate.rs      — LLM 巩固引擎: 6 类动作 + dream 采样 + 保护规则 (v1.2)
llm.rs              — MiniMax M3 / DeepSeek chat client + usage 采集
capacity.rs         — 容量驱逐链 / 摘要入口 degrade-restore / 冷压缩 (v1.3)
concepts.rs         — 概念表: 排序 + title 压缩 (v1.4)
embeddings.rs       — MiniMax embo-01 向量后端
bin/mcp.rs          — MCP stdio server
bin/cli.rs          — terminal CLI (clap)
```

### TS adapters (`packages/`)

```
mnemush-client/       — shared library to spawn mnemush binary + MCP RPC
mnemush-pi/           — Pi extension: 概念表注入(session_start + 写入刷新) + memory 工具
mnemush-opencode/     — OpenCode plugin (hooks + tools)
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
1. secret scanner (scanner.rs, blocks add on credential patterns)
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

Actual hook flow in the pi extension (`packages/mnemush-pi/src/index.ts`):

```
session_start:
  1. connect to mnemush-mcp (spawn subprocess)
  2. sendStatus("🧠 mnemush connected")
  3. surfacePendingIdentityProposals() — async, non-blocking
  4. surfaceReflectCandidateCount() — async, non-blocking
  Identity files (USER/PERSONA/CONSTITUTION) are loaded by the
  pi host from disk and injected into the system prompt (frozen
  for the session); mnemush doesn't re-read them.

before_agent_start (each user prompt):
  1. reset toolCallsThisTurn counter
  2. L1 heuristic capture on event.prompt:
     - looksLikeRemember(text) → memory_add(category=note, importance=0.9)
       and sendStatus("🧠 saved (remember)")
     - looksLikeCorrection(text) → memory_add(category=correction, importance=0.9)
       and sendStatus("🧠 saved (correction)")
  3. agent loop runs; agent may call memory tools on demand

after_tool_call (each tool result):
  Two listeners, both skip our own tools (Pi + OpenCode namespaces)
  so we never record our own failures as "tool failure" memories:
  skip list covers Pi names (`memory`, `memory_get`, ..., identity_*, ...)
  AND OpenCode names (`mnemush-memory`, `mnemush-memory_search`, ...).
  - L1 error capture: if result.is_error && result.error →
    memory_add(category=failure, importance=0.7, needs_review=true)
  - Periodic insight nudge: increment counter; at the 6th
    and 14th non-self tool call, sendStatus nudges the agent
    to use memory_add(category=insight, importance=0.7)

session_end:
  1. runPruneOnSessionEnd() — default `apply` (soft-delete up
     to MNEMUSH_PRUNE_SESSION_LIMIT candidates, default 5);
     set MNEMUSH_PRUNE_ON_SESSION_END=dry to list only, =off to skip.
     Hard-delete (`--isolate`) is NEVER auto-run.
  2. runEdgeDecayOnSessionEnd() — default ON; runs
     `mnemush edge-decay` which recomputes every active edge's
     strength via the same Ebbinghaus formula as memory confidence.
     Set MNEMUSH_EDGE_DECAY_ON_SESSION_END=off to skip. Without this
     pass, edge strength is monotonically non-decreasing, so the
     graph only grows noisier over time.
  3. runNeedsReviewOnSessionEnd() — default ON; runs
     `mnemush process-needs-review` which clears the `needs_review`
     flag on items older than MNEMUSH_NEEDS_REVIEW_GRACE_DAYS (1d
     default). For `category=failure` items, also downgrades
     importance by 0.1 per pass so repeated errors fade out.
     Set MNEMUSH_NEEDS_REVIEW_ON_SESSION_END=off to skip. Without
     this pass, the needs_review queue grows unboundedly (the
     after_tool_call error capture sets the flag but no consumer
     cleared it).
  4. client.disconnect() — close mnemush-mcp subprocess
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
~/.mnemush/
├── identity/
│   ├── USER.md
│   ├── PERSONA.md
│   └── CONSTITUTION.md
├── config.toml
├── mnemush.db              (mushroom_body: memory/edge/embedding/FTS/event)
├── mnemush.db-consolidate.json  (consolidate 增量位置, 随库隔离)
├── neuropils/              (默认 neuropil 目录, v1.1)
│   ├── archive/            (冷归档: 合并页 + tar.gz, v1.3)
│   └── <project>/…md
├── eval/                   (LLM 响应存档 + self-eval NDJSON)
└── pending.jsonl           (identity reflection proposals, transient)
```

### 容量与归档(Storage 之上, v1.3)

- **物理上限**: `[capacity] max_db_mb`(默认 100)由 `PRAGMA page_count×page_size` 触发(add 时),驱逐收敛按活数据估算(content + 活跃向量 + 活跃边)。
- **摘要入口**: neuropil 化记忆降级为前 2 句摘要 + `context=neuropil:<path>` 标记,全文在文件树。FTS 同步(旧全文不再命中)。
- **冷归档**: 30 天双条件(入口无命中 + 文件未改)→ 合并归档页 + tar.gz 打包。

## Memory types

| Type | Use | Decay | Example |
|---|---|---|---|
| **Identity** | user profile / agent persona | never | "user prefers Rust over Go" |
| **Procedural** | skills (how to do X) | normal | SKILL.md body |
| **Semantic** | facts, decisions, preferences, knowledge, episodic | normal | "auth uses jose not jsonwebtoken" |
| **ForgetTrace** | 遗忘痕迹: 记录一次主动遗忘(被遗忘者/原因) | normal, 可再遗忘 | "[forgotten] old FTP details — 过时" (v1.2) |

Episodic is not a separate type — it's `Semantic` with `category=Episodic` + a timestamp.

## Edge types

| Type | Direction | Use |
|---|---|---|
| `related` | bidirectional | generic association (default) |
| `supports` | bidirectional | A provides evidence for B |
| `contradicts` | bidirectional | A and B are in conflict |
| `supersedes` | source→target | A replaces B (memory evolution) |

额外 provenance 标记: `consolidate:link`(巩固建边)、`consolidate:insight`(顿悟连边)、`neuropil:wikilink`/`neuropil:copath`(文件树 wikilink/共现边)。

## Tunable parameters

See [config.example.toml](docs/config.example.toml) for the full schema. The most-tuned:

| Parameter | Default | What it does |
|---|---|---|
| `forgetting.half_life_days` | 90.0 | base half-life (importance=0.5) |
| `forgetting.prune_confidence_threshold` | 0.1 | below this → prune candidate |
| `forgetting.disable_forgetting` | false | true = archive mode (never forget) |
| `search.weight_importance` | 0.2 | how much importance affects ranking |
| `capacity.max_db_mb` | 100.0 | 物理上限(add 触发驱逐链) |
| `capacity.cold_days` | 30 | neuropil 冷判定天数 |
| `capacity.dream_sample_m` | 3 | dream 采样延伸度(≤10·m² 条/轮) |

## Performance targets

| Operation | Target (10K memories) | Target (1K) |
|---|---|---|
| Startup | < 500ms | < 100ms |
| FTS5 search | < 20ms | < 5ms |
| 2-hop neighbor (CTE) | < 50ms | < 10ms |
| Insert | < 5ms | < 2ms |
| Decay recalc (1K rows) | < 20ms | < 5ms |

## Future directions

See [ROADMAP.md](ROADMAP.md) for the planned v0.3+ scope.

## Why Rust + TS, not pure Rust

Pi and OpenCode agent runtimes are TypeScript. Their extension/plugin APIs are TypeScript SDKs. There is no Rust SDK for either.

Rust is the right choice for the core: single binary, fast startup, low memory, no runtime. TS is the right choice for the adapter layer: direct access to agent hooks, native JSON-RPC via stdio.

See [decisions.md](docs/decisions.md) for the full rationale.
