# 设计:概念表(context priming index)—— 记忆自动唤起的索引层

- **日期**: 2026-08-07
- **状态**: 已批准(brainstorming 逐节确认)
- **关联**: v1.0 语义召回(embedding)、v1.2 consolidate/dream、v1.3 容量管理;认知科学对应 —— 前额叶检索线索 + 海马体模式完成

## 背景与问题

mnemush 的记忆是**被动检索**:agent 只有显式调用 `search`/`memory` 工具才取回记忆。人类记忆是**自动激活**的:当前情境触及概念 → 海马体模式完成自动唤起相关记忆。被动检索的最大障碍不是"搜不到",而是 **"不知道有什么可搜"** —— agent 不知道记忆库里有 "FTS rowid 陷阱"、"MiniMax M3 重复" 这些可复用经验。

## 方案:概念表(唤起索引)

注入一个**简短概念索引**(title + category 列表)到 agent 上下文,让 agent 知道记忆库里有什么;需要细节时再显式检索。类比前额叶持有的检索线索,细节仍由海马体(显式 search)提取。

```
注入: 概念表(索引)                需要细节时
┌──────────────────────┐         ┌────────────┐
│ · GitHub proxy setup │  ────►  │ memory     │
│ · FTS rowid 陷阱     │         │ search     │ ← 显式提取(已有)
│ · MiniMax M3 重复    │         └────────────┘
└──────────────────────┘
   ~40 条 × ~30 token ≈ 1.2k token
```

## 组件

### 1. CLI 命令 `mnemush concepts [--limit N] [--format text|json]`

- **排序**: `score = importance × (1/(1+age_days/30)) × (1+ln(1+access_count))`
  - importance: 高价值经验优先
  - recency 因子: 只用 created_at(1/(1+age/30), 30 天半衰); 访问频率由 access_count 因子覆盖(不再用 last_accessed_at)
  - access 提升: 检索频繁的记忆优先
  - 全部基于既有字段(importance/created_at/access_count), 零新 schema
- **输出**:
  - text(默认): 每行 `压缩后 title (category)`, 排序降序
  - json: `{"concepts": [{"title","category","importance","score"}], "count": N}`
- **默认** `--limit 40`; 过滤: 活跃记忆(deleted_at IS NULL); 摘要入口(neuropil 化)也包含(其 title 仍是可检索概念)

### 1b. title 压缩(规则式, 零 LLM)

真实 title 47% 超 40 字符(subagent 任务描述/会话记录/贴入片段)。概念表是唤起索引, 不需要完整 title。

```
fn compress_title(t) -> String:
  1. 取第一行(按 \n 截断)
  2. 剥噪声前缀(依次, 以之开头则剥): "Task: " / "Task — " / "task: " /
     "你是 mnemush 项目的" / "你是 mnemush 项目" / "你是为 mnemush 项目" / "请"
     (注意顺序: "你是 mnemush 项目的" 必须在前, 先于 "你是 mnemush 项目")
  3. 长度 >= 48 字符 → take(48) + "…"(共 49)
  4. trim 首尾空白
```

- 48 字符 ≈ 概念表单条预算(中文 48 字 ≈ 25-35 token)
- 只影响概念表展示; memory 表原始 title 不动
- 新模块 `concepts.rs` 或并入 `capacity.rs`? —— 独立小模块 `concepts.rs`(职责单一, 可单测)

### 2. pi 插件注入

- **session_start**: 生成概念表 → 注入会话上下文(紧邻现有 identity 注入, packages/mnemush-pi/src/index.ts)
- **写入时刷新**: memory 写入(add/update 产生新记忆)后重新生成注入 —— 新经验即时可唤起
- 注入格式:
```
[memory index] 40 concepts (detail via memory tool):
· GitHub proxy setup (lesson)
· FTS rowid 陷阱 (lesson)
· MiniMax M3 重复 (tool_quirk)
...
```

### 3. 不改变的部分

- 现有 search/检索机制原样(概念表是引导层, 显式检索仍是提取路径)
- 不注入全文(只 title+category, 唤起用)
- 刷新仅发生在写入时(读操作不刷新, 成本可控)

## 大脑映射

| 概念 | 认知对应 |
|---|---|
| 概念表(索引) | 前额叶检索线索(index, 知道有什么) |
| memory search(显式) | 海马体模式完成/提取(detail) |
| 写入时刷新 | 编码后索引更新(新记忆进入检索线索) |

## 错误处理

- concepts 命令 DB 读取失败 → 报错(与现有 CLI 一致)
- 插件注入失败(session_start 无记忆/命令失败)→ 静默跳过, 不阻塞会话启动(与现有 identity 注入失败处理一致)
- 超长 title 截断(单条 > 80 字符 → 截断 + `…`), 防注入膨胀

## 测试面

- concepts 排序: importance/recency/access 各因子单独验证 + 综合
- 过滤: 软删/非活跃排除
- --limit / --format json
- 插件注入: session_start 注入包含概念表文本(TS 测试)

## 范围外(后续)

- 每轮上下文变化自动检索注入(方案 A: context-triggered recall)
- 扩散激活(priming): search 命中后邻居 touch
- 概念表聚类分组(按主题簇生成概念名)
