# 概念表(context priming index)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 注入简短概念索引到 agent 上下文,让 agent 知道记忆库有什么可搜(解决被动检索的"不知道有什么可搜"),模拟前额叶检索线索。

**Architecture:** Rust 侧新增 `mnemush concepts` 命令(importance×recency 排序 + title 压缩,零 LLM),pi 插件 session_start 注入 + 写入时刷新。复用既有 embedding/检索,不改变现有 search。

**Tech Stack:** Rust(SQLite), TypeScript(pi 插件, `sendMessage` 注入)。

## Global Constraints

- 编译/测试目录:`crates/mnemush/`;命令 `cd crates/mnemush && cargo test --release`。
- 二进制复制到 `~/.cargo/bin/` 后**必须 codesign**:`codesign --force --sign - ~/.cargo/bin/mnemush ~/.cargo/bin/mnemush-mcp`。
- 提交用 gitmoji:`✨`(新功能)/ `🐛`(bug)/ `🔧`(配置)/ `📝`(文档)。
- 注释/提示词中文,技术术语英文。
- **零 schema 改动**(排序用既有字段: importance/created_at/last_accessed_at/access_count)。
- title 压缩规则(spec 定稿): 取第一行 → 剥前缀("Task: "/"Task — "/"task: "/"你是 mnemush 项目"/"你是为 mnemush 项目"/"请")→ >48 字符截断 + "…" → trim。
- 排序公式(spec 定稿): `score = importance × (1/(1+age_days/30)) × (1+ln(1+access_count))`。

---

### Task 1: `mnemush concepts` 命令(排序 + title 压缩 + CLI)

**Files:**
- Create: `crates/mnemush/src/concepts.rs`(排序 + 压缩 + 查询)
- Modify: `crates/mnemush/src/lib.rs`(pub mod concepts)
- Modify: `crates/mnemush/src/bin/cli.rs`(Cmd::Concepts + 实现)
- Test: `crates/mnemush/src/concepts.rs`

**Interfaces:**
- Consumes: `MemoryApi::{list_in_project, get}`, `Store::now_ts()`, `Category::as_str`
- Produces: `pub struct ConceptEntry { title: String, category: String, importance: f32, score: f32 }`;`pub fn compress_title(t: &str) -> String`;`pub fn score(m: &Memory) -> f32`;`pub fn concepts(api: &MemoryApi, limit: usize) -> Result<Vec<ConceptEntry>>`
- CLI: `mnemush concepts [--limit N] [--format text|json]`(默认 limit 40, format text)

- [ ] **Step 1: 写失败测试**

`concepts.rs` 测试模块(helper `test_store()` 复制 consolidate 惯例):

```rust
#[test]
fn compress_title_strips_prefix_and_truncates() {
    assert_eq!(compress_title("Task: You are a delegated subagent running from a fork"), "You are a delegated subagent running from a fork…");
    assert_eq!(compress_title("你是 mnemush 项目的实现者, 完成 Task 3"), "实现者, 完成 Task 3");
    assert_eq!(compress_title("short title"), "short title");
    assert_eq!(compress_title("第一行\n第二行"), "第一行");
    let long = "x".repeat(100);
    assert_eq!(compress_title(&long).len(), 49, "48 chars + …");
}

#[test]
fn score_prefers_important_recent_accessed() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    // 重要 + 新 + 访问多 > 次要 + 旧 + 未访问
    let mut a = NewMemory::note("a", "important fresh");
    a.importance = 0.9;
    let id_a = api.add(a).unwrap().id;
    let mut b = NewMemory::note("b", "low old");
    b.importance = 0.1;
    let id_b = api.add(b).unwrap().id;
    // b 拨旧 100 天
    api.store.conn.execute("UPDATE memory SET created_at = ?1 WHERE id = ?2",
        rusqlite::params![crate::store::Store::now_ts() - 100*86400, id_b]).unwrap();
    let ma = api.get(&id_a).unwrap().unwrap();
    let mb = api.get(&id_b).unwrap().unwrap();
    assert!(score(&ma) > score(&mb), "important+fresh outranks low+old");
}

#[test]
fn concepts_filters_soft_deleted_and_orders() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let mut hi = NewMemory::note("top concept", "x");
    hi.importance = 0.9;
    api.add(hi).unwrap();
    let mut lo = NewMemory::note("bottom concept", "y");
    lo.importance = 0.1;
    let id_lo = api.add(lo).unwrap().id;
    api.soft_delete(&id_lo).unwrap(); // 软删应排除
    let c = concepts(&api, 10).unwrap();
    assert_eq!(c.len(), 1, "soft-deleted excluded");
    assert_eq!(c[0].title, "top concept");
    assert!(c[0].score > 0.0);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release concepts`
Expected: FAIL(模块不存在)

- [ ] **Step 3: 实现 concepts.rs**

```rust
//! concepts —— 概念表(context priming index): 排序 + title 压缩。
//! 给 agent 的唤起索引(知道记忆库有什么可搜), 零 LLM。

use crate::error::Result;
use crate::memory::MemoryApi;
use crate::schema::Memory;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConceptEntry {
    pub title: String,
    pub category: String,
    pub importance: f32,
    pub score: f32,
}

const TITLE_MAX: usize = 48;
const NOISE_PREFIXES: &[&str] = &[
    "Task: ", "Task — ", "task: ",
    "你是 mnemush 项目", "你是为 mnemush 项目", "请",
];

/// 规则压缩 title(零 LLM): 第一行 → 剥前缀 → 48 字符截断 → trim。
pub fn compress_title(t: &str) -> String {
    let mut out = t.split('\n').next().unwrap_or("").trim().to_string();
    for p in NOISE_PREFIXES {
        if out.starts_with(p) {
            out = out[p.len()..].trim_start().to_string();
            break;
        }
    }
    if out.chars().count() > TITLE_MAX {
        out = out.chars().take(TITLE_MAX - 1).collect::<String>() + "…";
    }
    out
}

/// 排序分: importance × recency(30 天半衰) × access 提升。
pub fn score(m: &Memory) -> f32 {
    let age_days = ((crate::store::Store::now_ts() - m.created_at.timestamp()).max(0) as f32) / 86400.0;
    let recency = 1.0 / (1.0 + age_days / 30.0);
    let access = 1.0 + (1.0 + m.access_count as f32).ln();
    m.importance * recency * access
}

/// top-N 概念(活跃记忆, 含摘要入口), 按 score 降序。
pub fn concepts(api: &MemoryApi, limit: usize) -> Result<Vec<ConceptEntry>> {
    let all = api.list_in_project(100_000, None)?;
    let mut out: Vec<ConceptEntry> = all
        .into_iter()
        .filter(|m| m.deleted_at.is_none())
        .map(|m| ConceptEntry {
            title: compress_title(&m.title),
            category: m.category.as_str().to_string(),
            importance: m.importance,
            score: score(&m),
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}
```

- [ ] **Step 4: CLI 命令(cli.rs)**

enum Cmd 加(Search 前):

```rust
/// List top concepts (memory index) for agent priming.
Concepts {
    #[arg(long, default_value_t = 40)]
    limit: usize,
    /// text (default) or json.
    #[arg(long)]
    format: Option<String>,
},
```

match 分支:

```rust
Cmd::Concepts { limit, format } => {
    let api = MemoryApi::new(&store, &config);
    let list = mnemush::concepts::concepts(&api, limit)?;
    match format.as_deref() {
        Some("json") => println!("{}", serde_json::to_string(&list)?),
        _ => {
            for c in &list {
                println!("· {} ({})", c.title, c.category);
            }
        }
    }
}
```

- [ ] **Step 5: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿(166+18 基线 + 3 新)

- [ ] **Step 6: 提交**

```bash
git add crates/mnemush/src/concepts.rs crates/mnemush/src/lib.rs crates/mnemush/src/bin/cli.rs
git commit -m "✨ concepts: 概念表命令(importance×recency 排序 + title 压缩)"
```

---

### Task 2: pi 插件注入(session_start + 写入时刷新)

**Files:**
- Modify: `packages/mnemush-pi/src/index.ts`
- Test: `packages/mnemush-pi/test/`(新建或扩展)

**Interfaces:**
- Consumes: Task 1 `mnemush concepts --limit 40`(CLI 调用)或通过 MCP(若已有工具则复用; 无则用 child_process 调 CLI)
- Produces: session_start 注入 `[memory index] N concepts (detail via memory tool):\n· ...`;after_tool_call 检测 memory 写入 → 重新注入

- [ ] **Step 1: 写失败测试(TS)**

`packages/mnemush-pi/test/concepts.test.mjs`:

```js
import { test } from "node:test";
import assert from "node:assert";
// 生成注入文本的函数(从 index.ts 导出或抽到模块)
import { buildConceptInject } from "../src/index.ts"; // 若可导入; 否则测试经 CLI

test("buildConceptInject formats concept table", () => {
  const inject = buildConceptInject([
    { title: "GitHub proxy setup", category: "lesson", importance: 0.9, score: 1.2 },
    { title: "FTS rowid 陷阱", category: "lesson", importance: 0.8, score: 1.1 },
  ]);
  assert.match(inject, /\[memory index\] 2 concepts/);
  assert.match(inject, /· GitHub proxy setup \(lesson\)/);
  assert.match(inject, /detail via memory tool/);
});
```

(若 index.ts 不可直接导入 ES module,抽 `buildConceptInject` 到 `packages/mnemush-pi/src/concepts.ts` 导出。)

- [ ] **Step 2: 运行确认失败**

Run: `cd packages/mnemush-pi && npm test`
Expected: FAIL(函数不存在)

- [ ] **Step 3: 实现注入函数**

`packages/mnemush-pi/src/concepts.ts`:

```ts
export interface ConceptEntry {
  title: string;
  category: string;
  importance: number;
  score: number;
}

/** 概念表注入文本: 唤起索引(详情走 memory 工具)。 */
export function buildConceptInject(concepts: ConceptEntry[]): string {
  if (concepts.length === 0) return "";
  const lines = concepts.map((c) => `· ${c.title} (${c.category})`).join("\n");
  return `[memory index] ${concepts.length} concepts (detail via memory tool):\n${lines}`;
}
```

- [ ] **Step 4: 插件接线(index.ts)**

```ts
// session_start 内(连接成功后, identity 注入附近):
const inject = await loadConceptInject(); // 调 `mnemush concepts --limit 40 --format json` 解析
if (inject) pi.sendMessage?.(inject);     // 注入会话上下文

// after_tool_call: 检测 memory 写入 → 刷新
pi.on("after_tool_call", async (event: any) => {
  const tool = event?.toolCall?.name ?? "";
  if (tool.startsWith("memory_add") || tool === "mnemush_memory_add") {
    const inject = await loadConceptInject();
    if (inject) pi.sendMessage?.(inject);
  }
});
```

`loadConceptInject` 用 child_process 调 `mnemush concepts --format json`(参考 index.ts 既有 `spawn("mnemush", ...)` 模式, index.ts:283),解析 JSON 数组 → `buildConceptInject`。失败静默返回 null(不阻塞会话)。

- [ ] **Step 5: 运行测试**

Run: `cd packages/mnemush-pi && npm test`
Expected: PASS

- [ ] **Step 6: build + 端到端冒烟**

```bash
cd packages/mnemush-pi && npm run build
# 手动验证: 临时会话观察 session_start 是否注入概念表(或单测覆盖注入文本)
cd crates/mnemush && cargo build --release && cp target/release/mnemush ~/.cargo/bin/ && codesign --force --sign - ~/.cargo/bin/mnemush
```

- [ ] **Step 7: 更新 CHANGELOG + README**

CHANGELOG 加 v1.4.0 段(概念表);README Status 补。

- [ ] **Step 8: 提交**

```bash
git add packages/mnemush-pi CHANGELOG.md README.md
git commit -m "✨ pi: 概念表注入(session_start + 写入时刷新)"
```

---

## Self-Review

**Spec 覆盖:**
- `mnemush concepts [--limit N]`(排序/过滤/压缩/json)→ Task 1 ✓
- title 压缩规则(前缀剥离 + 48 截断)→ Task 1 compress_title ✓
- 排序公式(importance × recency × access)→ Task 1 score ✓
- pi 插件 session_start 注入 → Task 2 ✓
- 写入时刷新 → Task 2 after_tool_call ✓
- 零 schema 改动 / 不改变现有检索 → 全计划 ✓
- 错误处理(注入失败静默)→ Task 2 ✓

**占位符:** 无 TBD/TODO;每步含真实代码。

**类型一致性:** `ConceptEntry{title,category,importance,score}`(Task 1 Rust)与 `ConceptEntry`(Task 2 TS)字段一致;`compress_title`/`score`/`concepts` 签名一致;CLI `--limit/--format` 与插件调用参数一致(`mnemush concepts --limit 40 --format json`)。
