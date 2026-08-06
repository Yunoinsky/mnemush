# 设计:v1.2 文件树记忆(文件=源, mnemush=关系层)

- **日期**: 2026-08-07
- **状态**: 已批准(brainstorming 逐节确认)
- **关联**: 2026 记忆架构调研(Filesystem-Based Memory for LLM Agents, arXiv:2607.26637;Tenure Crossover, arXiv:2607.21962)

## 背景与动机

2026 实证研究(arXiv:2607.26637 等)表明:文件系统树状记忆的稳定收益是**检索成本**(材料大时搜索成本腰斩),而图/向量结构擅长非树状关联。动物记忆系统提供了统一视角:**皮层各子脑区维护树状内容存储(文件系统)**,**海马/蘑菇体维护跨簇的关联索引(图结构)**。

mnemush 现有:SQLite(`memory` + `memory_edge` related/supports/contradicts/supersedes)+ FTS5 + MiniMax 向量语义召回。**缺少树状内容层**——记忆内容不可直接翻阅/编辑,只能通过 CLI/MCP 工具访问(每次调用有进程/JSON-RPC 开销)。

本设计引入文件树内容层:记忆内容以 markdown 文件为权威源(Agent 可用 `grep`/`awk`/`tree` 免费读取、Git 版本化),mnemush 维护非树状关系层(FTS + 向量 + 边)。

## 命名约定(动物记忆映射)

- **neuropils(神经毯)** — 文件树内容层:`~/.mnemush/neuropils/` 目录、`project=neuropils` 隔离、模块 `neuropils.rs`、tag 前缀 `neuropil-path:`、provenance `neuropil:wikilink` / `neuropil:copath`
- **mushroom_body(蘑菇体/蕈状体)** — 图索引层:memory_edge(related/supports/contradicts/supersedes)+ 向量,跨簇关联的索引与联想;import 输出报告 `mushroom_body edges: N`
- **任意目录树都可作为 neuropil 导入**(`import-tree <dir> --project <name>`);默认 neuropils。**external-wiki 是既有 neuropil 实例**:wiki markdown 页=内容层,`project=external-wiki` 索引=mushroom_body,其 import/link 逻辑(scripts/import_wiki.py、link_wiki.py)与 neuropil 文件格式(wikilink + frontmatter)同构,后续可统一到 import-tree(本次不做迁移)

## 架构

```
~/.mnemush/neuropils/            ← 记忆文件树(内容权威;grep/cat/tree/Git 直接可用)
├── lesson/
│   └── proxy/
│       └── github-clash-7890.md    ← frontmatter: title/category/tags/links: [[...]]
├── decision/
│   └── rename/
│       └── mneme-to-mnemush.md
└── ...

SQLite (~/.mnemush/mnemush.db)  ← 关系层(海马/蘑菇体): FTS5 + 向量 + memory_edge
```

**职责划分**(与动物类比的映射):
- 文件树 = 皮层子脑区:树状层级由文件系统天然提供,人/Agent 直接可读可编辑
- SQLite 边+向量 = 海马/蘑菇体:跨簇连接(树装不下的关联),语义召回
- shell 工具(`grep`/`cat`/`tree`) = 低成本直接读取;`mnemush search` = 语义/关联检索

## 数据流

**写入**: 人/Agent 编辑 markdown 文件(或 `mnemush add` 生成文件);文件是唯一内容源。

**索引**: `mnemush import-tree` 增量扫描(按内容 hash 幂等)→ 同步 FTS + 向量 → 建边:
- 显式: frontmatter `links: [[topic/file]]` 或正文 wikilink → related/supports 边(人可控、可审计)
- 自动: 内容相似度(复用现有 auto-link)+ 向量近邻 + 同目录共现

**读取双通道**:
- 路径已知 / 精确匹配 → shell 工具(grep/cat/tree),零额外调用成本
- 语义/关联/模糊 → `mnemush search`(现有 FTS+向量+边,含语义召回)

## CLI 设计

**新增命令**:
- `mnemush import-tree [dir]` — 默认 `~/.mnemush/neuropils/`;增量同步文件树 → 索引+边;幂等(内容 hash),文件编辑后重跑即更新;以 `project=neuropils` 隔离
- `mnemush export-tree [dir]` — 现有 SQLite 记忆一次性落盘成文件树(权威迁移,供已有记忆使用)

**保持兼容**(全部不动): `add`/`search`/`delete`/`prune` + MCP 工具链。`add` 仍写 SQLite —— 与文件树记忆**双轨共存**:文件树记忆走 import-tree,CLI 记忆走原路径;两者在 SQLite 内统一索引,边可互连(文件记忆的 wikilink 可指向 CLI 记忆 id 前缀)。

**同步策略**: 手动 `import-tree`(幂等可反复跑);watch 自动监听留作后续(论文 RQ4 表明整理收益有限,先不做)。

**命名**: 内容层 = **neuropils**(`~/.mnemush/neuropils/`,`project=neuropils`);索引层 = **mushroom_body**(memory_edge + 向量);见 spec「命名约定」节。

## 文件格式约定

每个记忆文件:
```markdown
---
title: GitHub proxy setup
category: lesson
tags: [proxy, github]
links: ["../decision/rename/mneme-to-mnemush.md"]
---
# GitHub proxy setup

Accessing GitHub requires the Clash proxy at 127.0.0.1:7890...
```

- `links` 支持相对路径 wikilink:`[[topic/file]]`(精确)或 `[[title]]`(需在文件树内唯一匹配,重名时须用路径形式)
- 目录结构即层级提示;frontmatter 的 `category`/`topic` 为权威字段
- 内容 hash(与现有 dedup 同一机制)驱动增量同步
- `export-tree` 导出的文件成为该批记忆的权威源;此后编辑文件 + `import-tree` 更新;若同时用 `add` 修改同一记忆,以最后一次写作为准(双轨并存,不做冲突检测)

## 兼容性与迁移

- 现有 5632 条 SQLite 记忆:`export-tree` 一次性落盘为文件树,之后文件为源;SQLite 索引保留
- 既有 add/search/MCP 全部不动
- 文件树记忆与 CLI 记忆可互相建边(统一索引层)

## 测试

1. `import-tree` 幂等:重跑不产生重复记忆/边
2. wikilink → 边正确映射(显式 related/supports)
3. 编辑文件后 reimport:内容更新,旧索引不残留
4. `export-tree` → `import-tree` 往返一致(内容 hash 稳定)
5. 自动边:同目录共现 + 内容相似度触发,不误连跨簇(复用现有 auto-link 阈值)

## 非目标(本次不做)

- watch 自动同步(留待有实际需求)
- `add` 改造为写文件(双轨共存,不动现有路径)
- 文件级权限/加密(未要求)
- 多用户冲突解决(未要求)

## 参考

- Filesystem-Based Memory for LLM Agents: Organization, Evolution, and Sustainability (arXiv:2607.26637)
- Ground Truth First: ... The Tenure Crossover in Memory-Architecture Rankings (arXiv:2607.21962)
- Are We Ready For An Agent-Native Memory System? (arXiv:2606.24775)
- mnemush 现有:external-wiki 双层模式(scripts/import_wiki.py, scripts/link_wiki.py)
