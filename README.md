# Mnemush 🧠

> Persistent, brain-inspired memory for AI coding agents. Rust core, TS adapters for Pi and OpenCode.

**Mnemush** is a portmanteau of **mneme** (Greek μνήμη, "memory") and **mushroom** — a nod to the insect **mushroom body** (蕈体 / 蘑菇体), the brain structure responsible for learning and memory in insects (flies, bees, ants). Just as the mushroom body stores sparse, distributed, associative memories that let an insect generalize across contexts, Mnemush keeps your agent's memories as a linked graph that auto-consolidates — distributed associative storage, with the "fruiting body" being the retrievable memory that emerges from the network when it matters.

## Architecture in one minute

Two layers, mirroring insect neurobiology:

```
 neuropils(内容层, 文件树)                    mushroom_body(索引层, 主库)
┌─────────────────────────────┐            ┌──────────────────────────────┐
│ 任意 markdown 文件树 = 记忆   │            │ agent 经验: 全文 + 向量 + 图    │
│ 内容权威源(grep/cat/Git 可读) │  import   │ 摘要入口: neuropil 化的记忆留   │
│                             │◄──────────►│  title+摘要+路径(内容在文件树)  │
└─────────────────────────────┘  按需加载   └──────────────────────────────┘
```

- **neuropils**(神经毯) — 文件树内容层。任何目录树都是记忆(概念、论文、知识库),文件即权威源,可用 grep/cat/tree 直接读、Git 版本化。`mnemush import-tree <dir> --project <name>` 增量同步进索引。
- **mushroom_body**(蘑菇体) — 主库(SQLite + FTS5 + 向量)。存 agent 经验的全文/向量/图、神经pil 化的摘要入口、以及跨簇关联边。检索、巩固、遗忘都在这一层。

```
        ┌────────────┐   ┌────────────┐
        │    Pi      │   │  OpenCode  │   (TS agents)
        └─────┬──────┘   └─────┬──────┘
              │ mnemush-pi/opencode (hooks + tools)
              └────────┬───────┘
                       │ MCP stdio
                       ▼
              ┌─────────────┐
              │ mnemush (Rust) │  ← single binary, MCP server + CLI
              └──────┬──────┘
                     ▼
        ~/.mnemush/mnemush.db   (mushroom_body)
        ~/.mnemush/neuropils/   (默认 neuropil 目录)
```

**大脑映射**:neuropils = 皮层(内容就地存储),mushroom_body = 海马/蘑菇体(索引 + 关联);consolidate = 记忆巩固,dream = 睡眠期巩固+遗忘高峰,概念表 = 前额叶检索线索,forget_trace = 遗忘痕迹(忘掉什么本身也是信息)。

## Status

**v1.4.0 (2026-08-07)** — 概念表(context priming index): `mnemush concepts` 按 importance×recency×access 输出 top-N 唤起索引;Pi 插件 session_start 注入 + 写入时刷新。让 agent 知道记忆里有什么可搜(模拟前额叶检索线索)。

**v1.3.0 (2026-08-07)** — 容量管理: 物理 100MB 硬阈值 + 驱逐链 + neuropil 化 + 冷归档,并入 nightly dream。

**v1.2.0 (2026-08-07)** — LLM 驱动巩固 + 主动遗忘: `mnemush consolidate` / `mnemush dream`(遗忘 + 遗忘痕迹)。

**v1.1.0 (2026-08-07)** — neuropils 文件树记忆: `mnemush import-tree` / `export-tree`,任意目录树即记忆。

**v1.0.0 (2026-08-06)** — 稳定版: API 稳定性、跨平台 CI、语义召回、自动合并、Git 同步。

**v0.4 (2026-08-05)** — backup/restore、多项目隔离、schema 迁移。**v0.3** — 图分析 + 自评估。**v0.1-0.2** — 核心存储 + MCP + 自适应维护。

详见 [CHANGELOG.md](CHANGELOG.md) 与 [ROADMAP.md](ROADMAP.md)。

## Quick start

```bash
# Clone & enter
git clone https://github.com/Yunoinsky/mnemush.git && cd mnemush

# Install (builds Rust + TS, copies to ~/.cargo/bin, inits ~/.mnemush/)
./scripts/install.sh

# Configure
$EDITOR ~/.mnemush/config.toml
$EDITOR ~/.mnemush/identity/USER.md

# Try the CLI
mnemush add "use jose not jsonwebtoken" --category decision --importance 0.9
mnemush search "jose"
mnemush list
mnemush status                    # 含容量段: DB 大小/上限 + neuropil 入口数

# v1.1: neuropils — 任何目录树都是记忆
mnemush import-tree ~/my-knowledge --project wiki   # 索引文件树(前端matter + wikilink)
mnemush export-tree ~/out --project wiki            # 导出回文件树

# v1.2: LLM 驱动巩固 + 主动遗忘
mnemush consolidate --dry-run     # 预览 LLM 会做什么
mnemush consolidate               # 增量巩固: update/link/merge/insight/decay/forget
mnemush dream                     # nightly 全库: 巩固 + neuropil 化 + 冷压缩 + 容量报告

# v1.4: 概念表(唤起索引)
mnemush concepts --limit 40       # top-N 概念, 注入 agent 上下文
```

## For Pi / OpenCode

```bash
# Pi extension (from your local clone)
npm install -g /path/to/mnemush/packages/mnemush-pi
# 或软链到 ~/.pi/agent/extensions/
# 重启 pi 后: session_start 注入概念表, memory 工具可用, 会话自动捕获
```

- Pi 插件在 `session_start` 注入 `[memory index] N concepts` 唤起索引,memory 写入后自动刷新
- 启发式捕获:corrections、"remember X"、工具错误自动入库
- `mnemush-worker` agent 可独立使用全套 memory 工具

## Features

- **两层记忆架构** — neuropils(文件树内容)+ mushroom_body(索引/图)
- **语义检索** — MiniMax embo-01 向量 + FTS5 混合(中文↔英文零重叠可命中)
- **图结构 LTM** — 记忆互连,add 时 auto-link;PageRank/社区发现
- **LLM 巩固 + 主动遗忘** — consolidate/dream,双阈值 + 保护规则(importance≥0.7/never_prune/identity/7天)
- **容量自治** — 100MB 硬阈值驱逐链 + neuropil 化 + 冷归档打包
- **概念表唤起** — agent 上下文常驻记忆索引
- **身份层** — USER/PERSONA/CONSTITUTION 每会话注入
- **单二进制** — ~5-12 MB,无 Python/Docker/云

## Configuration

所有参数在 `~/.mnemush/config.toml`(示例见 [docs/config.example.toml](docs/config.example.toml)):

- `[forgetting]` — half-life、prune 阈值、访问提升
- `[capacity]` — `max_db_mb`(物理上限)、`cold_days`(冷判定)、`dream_sample_m`(dream 采样延伸度)
- `[embedding]` — 语义检索开关 + MiniMax 模型
- `[project]` — 多项目隔离(MNEMUSH_PROJECT)
- `[edges]` — auto-link/auto-merge 阈值

## Project layout

```
crates/mnemush/        — Rust core(binary + lib)
  src/neuropils.rs     — 文件树导入/导出(内容层)
  src/consolidate.rs   — LLM 巩固 + dream 引擎
  src/capacity.rs      — 容量驱逐/摘要入口/冷压缩
  src/concepts.rs      — 概念表排序 + title 压缩
  src/llm.rs           — MiniMax/DeepSeek 聊天客户端
  src/memory.rs        — add/search/get/update + 语义召回
  src/embeddings.rs    — MiniMax embo-01 向量
  src/edge.rs          — 图边 + BFS 邻居
  src/forget.rs        — 遗忘曲线 + prune + 遗忘痕迹
packages/mnemush-pi/   — Pi 扩展(概念表注入 + memory 工具)
packages/mnemush-opencode/ — OpenCode 插件
packages/mnemush-client/   — 共享 TS 客户端
docs/                  — 架构/决策/配置示例/superpowers 设计档案
```

## Development

```bash
# Rust tests
cargo test --manifest-path crates/mnemush/Cargo.toml

# Build everything
npm run build --workspaces

# Run CLI
cargo run --manifest-path crates/mnemush/Cargo.toml -- search "jose"

# Run MCP server directly (for testing)
cargo run --manifest-path crates/mnemush/Cargo.toml --bin mnemush-mcp
```

187 Rust tests green at HEAD (169 lib + 18 bin), plus 3 TS extension tests.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — 架构与模块
- [docs/decisions.md](docs/decisions.md) — 设计决策记录
- [docs/config.example.toml](docs/config.example.toml) — 配置参考
- [docs/superpowers/](docs/superpowers/) — 设计档案(specs + plans)

## License

[MulanPSL-2.0](LICENSE)
