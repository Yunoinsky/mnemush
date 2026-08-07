# v1.3 记忆容量管理 + neuropil 归档 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 三层容量治理 —— 物理 ≤100MB 硬阈值、条数交 LLM 遗忘、neuropil 化/压缩控制长期知识驻留。

**Architecture:** neuropils(文件树内容层)↔ mushroom_body(主库)。wiki 动态索引(局部 import/清理)+ neuropil 化(export + 摘要入口)+ neuropil 压缩(冷判定 + 合并 + tar.gz 打包)+ 100MB 驱逐链。全部并入 dream 每日流程。

**Tech Stack:** Rust, SQLite(rusqlite), FTS5, tar 0.4 + flate2(backup 已依赖)。

## Global Constraints

- 编译/测试目录:`crates/mnemush/`(Cargo.toml 在此,非仓库根)。命令:`cd crates/mnemush && cargo test --release` / `cargo build --release`。
- 二进制复制到 `~/.cargo/bin/` 后**必须 codesign**:`codesign --force --sign - ~/.cargo/bin/mnemush ~/.cargo/bin/mnemush-mcp`(macOS AMFI 杀未签名二进制)。
- 提交用 gitmoji:`✨`(新功能)/ `🐛`(bug)/ `🔧`(配置)/ `📝`(文档)/ `♻️`(重构)/ `🩹`(小修)。
- 注释/提示词用中文,技术术语保持英文。
- **零 schema 改动**(spec 约束):neuropil 路径存 `Memory.context` 字段,不加新列。
- 新记忆受保护规则:importance≥0.7 / never_prune / identity / 7 天内 → 禁 decay/forget(consolidate.rs `is_protected`)。
- 摘要截取为规则(前 2 句),**不用 LLM 生成**。

---

### Task 1: 容量配置 + 访问记录(search 命中 touch last_accessed_at)

冷判定依赖"入口 30 天无命中"—— 但 search 目前**不更新** `last_accessed_at`(只有 add 初始化)。本任务补上,并加容量配置结构。

**Files:**
- Modify: `crates/mnemush/src/config.rs`(新增 `CapacityConfig`)
- Modify: `crates/mnemush/src/config.rs`(Config 加 `capacity` 字段)
- Modify: `crates/mnemush/src/memory.rs`(search 命中后 UPDATE last_accessed_at/access_count)
- Modify: `crates/mnemush/docs/config.example.toml`(capacity 段)
- Test: `crates/mnemush/src/memory.rs`(search touch 测试)

**Interfaces:**
- Produces: `Config::capacity: CapacityConfig`;`CapacityConfig { max_db_mb: f64 = 100.0, cold_days: i64 = 30, entry_summary_chars: usize = 300, eviction_batch: usize = 100 }`
- Produces: search 命中记忆的 `last_accessed_at` 被更新(后续冷判定读它)

- [ ] **Step 1: 写失败测试(search 命中更新 access)**

在 `memory.rs` 测试模块加:

```rust
#[test]
fn search_hit_records_access() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let id = api.add(NewMemory::note("needle content here", "needle")).unwrap().id;
    let before = api.get(&id).unwrap().unwrap().last_accessed_at;
    let hits = api.search("needle", SearchOpts { limit: 5, ..Default::default() }).unwrap();
    assert!(hits.iter().any(|h| h.memory.id == id), "search finds it");
    let after = api.get(&id).unwrap().unwrap();
    assert!(after.last_accessed_at > before, "last_accessed_at bumped");
    assert_eq!(after.access_count, 1, "access_count incremented");
}
```

(测试用 `test_store()`/`NewMemory::note`/`SearchOpts`,均已有。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release search_hit_records_access`
Expected: FAIL(`last_accessed_at` 不变)

- [ ] **Step 3: 实现 —— search 命中 touch**

在 `memory.rs` 的 `search()` 收集命中后、返回前,批量 UPDATE:

```rust
// 命中记录访问(容量冷判定依赖 last_accessed_at)
{
    let mut stmt = self.store.conn.prepare(
        "UPDATE memory SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2",
    )?;
    for h in &out {
        stmt.execute(rusqlite::params![Store::now_ts(), h.memory.id])?;
    }
}
```

(定位:`search()` 返回 `out` 之前。若已有类似 touch 逻辑则跳过本步并注释说明。)

- [ ] **Step 4: 配置结构**

`config.rs` 加:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityConfig {
    /// 物理上限 MB(add 时检查, 超限触发驱逐)。
    pub max_db_mb: f64,
    /// neuropil 冷判定: 入口多少天无命中 + 文件未改。
    pub cold_days: i64,
    /// 摘要入口截取字符数(规则截取 content 前 N 字符)。
    pub entry_summary_chars: usize,
    /// 驱逐每批处理条数。
    pub eviction_batch: usize,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            max_db_mb: 100.0,
            cold_days: 30,
            entry_summary_chars: 300,
            eviction_batch: 100,
        }
    }
}
```

`Config` 加字段 `pub capacity: CapacityConfig,` + Default 里 `capacity: CapacityConfig::default(),`。反序列化若用 `#[serde(default)]` 需给新字段加;检查现有 Config 是否整表 default。

- [ ] **Step 5: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿(新增测试 PASS)

- [ ] **Step 6: 更新 config.example.toml**

加:

```toml
[capacity]
max_db_mb = 100.0
cold_days = 30
entry_summary_chars = 300
eviction_batch = 100
```

- [ ] **Step 7: 提交**

```bash
git add crates/mnemush/src/config.rs crates/mnemush/src/memory.rs crates/mnemush/docs/config.example.toml
git commit -m "🔧 capacity: 配置结构 + search 命中记录访问(冷判定前置)"
```

---

### Task 2: 摘要入口 —— 记忆降级/恢复(degrade/restore)

neuropil 化后主库留摘要入口(id/title/摘要/路径/边, 无全文无向量); 需要时恢复。

**Files:**
- Create: `crates/mnemush/src/capacity.rs`(容量模块: 摘要截取 + 降级/恢复 + 驱逐 + 冷判定, 本任务先建摘要/降级/恢复)
- Modify: `crates/mnemush/src/lib.rs`(pub mod capacity)
- Test: `crates/mnemush/src/capacity.rs`

**Interfaces:**
- Consumes: `MemoryApi::{add, get, update, soft_delete}`, `Store::delete_embeddings_for`
- Produces: `pub fn entry_summary(content: &str, max_chars: usize) -> String` —— 截取前 2 句(按 `。.!?` 切, 超 max_chars 截断加 `…`);`pub fn degrade_to_entry(api, id, path) -> Result<()>`;`pub fn restore_from_entry(api, id, content) -> Result<()>`
- 约定: neuropil 路径存 `Memory.context`(`Some("neuropil:<rel_path>")`), 标记来源。

- [ ] **Step 1: 写失败测试**

`capacity.rs` 测试模块:

```rust
#[test]
fn entry_summary_takes_first_two_sentences() {
    let s = entry_summary("第一句。第二句。第三句。", 300);
    assert_eq!(s, "第一句。第二句。");
    let s2 = entry_summary("No punctuation here at all", 10);
    assert!(s2.len() <= 13, "truncated with ellipsis");
}

#[test]
fn degrade_to_entry_clears_content_keeps_path() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let id = api.add(NewMemory::note("full content here for a concept", "概念")).unwrap().id;
    degrade_to_entry(&api, &id, "neuropils/concepts/概念.md").unwrap();
    let m = api.get(&id).unwrap().unwrap();
    assert!(m.content.is_empty(), "content cleared");
    assert_eq!(m.context.as_deref(), Some("neuropil:neuropils/concepts/概念.md"));
    assert!(m.title.contains("概念"), "title kept");
}

#[test]
fn restore_from_entry_puts_content_back() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let id = api.add(NewMemory::note("full", "t")).unwrap().id;
    degrade_to_entry(&api, &id, "p.md").unwrap();
    restore_from_entry(&api, &id, "restored full body").unwrap();
    let m = api.get(&id).unwrap().unwrap();
    assert_eq!(m.content, "restored full body");
    assert!(m.context.is_none() || !m.context.as_deref().unwrap().starts_with("neuropil:"), "path marker cleared");
}
```

(测试 helper:`test_store()` 返回 `(Store, Config)`,已在 consolidate.rs;此处需在 capacity.rs 复制小 helper 或放公共 test 模块 —— 直接复制 `test_store`/`NewMemory::note` 用法即可。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release capacity::tests`
Expected: FAIL(函数未定义)

- [ ] **Step 3: 实现**

```rust
//! capacity —— 记忆容量管理: 摘要入口 / 驱逐 / 冷判定。
use crate::error::Result;
use crate::memory::MemoryApi;
use crate::store::Store;

/// 规则截取前 2 句; 无句号则按 max_chars 截断加省略号。
pub fn entry_summary(content: &str, max_chars: usize) -> String {
    let mut chars = content.chars().peekable();
    let mut out = String::new();
    let mut sentences = 0;
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '。' || c == '！' || c == '？' || c == '.' || c == '!' || c == '?' {
            sentences += 1;
            if sentences >= 2 || out.chars().count() >= max_chars {
                break;
            }
        }
        if out.chars().count() >= max_chars {
            break;
        }
    }
    if out.chars().count() >= max_chars && !out.ends_with('。') && !out.ends_with('.') {
        out.push('…');
    }
    out
}

/// 降级为摘要入口: 清全文与向量, 保留 title/摘要/路径(context=neuropil:path)/边。
/// 摘要存入 content(仅截取), 需要全文时按 context 路径从文件树读。
pub fn degrade_to_entry(api: &MemoryApi, id: &str, path: &str) -> Result<()> {
    let Some(mut m) = api.get(id)? else { return Ok(()); };
    if m.content.is_empty() {
        m.context = Some(format!("neuropil:{path}"));
        api.update(&m)?;
        return Ok(());
    }
    let summary = entry_summary(&m.content, 300);
    let cfg = &api.config.capacity;
    let summary = entry_summary(&m.content, cfg.entry_summary_chars);
    m.content = summary;
    m.context = Some(format!("neuropil:{path}"));
    m.content_hash = MemoryApi::content_hash(&summary);
    api.update(&m)?;
    // 删除旧向量(摘要重新 embed 由调用方决定; 这里只清全文级向量)
    api.store.delete_embeddings_for(id)?;
    Ok(())
}

/// 从文件树恢复全文(neuropil 化反向)。
pub fn restore_from_entry(api: &MemoryApi, id: &str, content: &str) -> Result<()> {
    let Some(mut m) = api.get(id)? else { return Ok(()); };
    m.content = content.to_string();
    m.content_hash = MemoryApi::content_hash(content);
    if let Some(ctx) = m.context.as_deref() {
        if ctx.starts_with("neuropil:") {
            m.context = None;
        }
    }
    api.update(&m)?;
    Ok(())
}
```

注意:`MemoryApi::new` 后 `api.config.capacity` 需 Task 1 的 `Config.capacity` 存在;`api.store.delete_embeddings_for` 是 `embeddings.rs` 里 `impl Store` 的方法(已存在)。

- [ ] **Step 4: 运行测试**

Run: `cd crates/mnemush && cargo test --release capacity::tests`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/capacity.rs crates/mnemush/src/lib.rs
git commit -m "✨ capacity: 摘要入口(降级/恢复), neuropil 路径存 context"
```

---

### Task 3: 容量驱逐 —— 评分 + 驱逐链 + add 触发

超 100MB 硬阈值时: ①清 wiki 临时索引(可再生) → ②低分 agent 软删 → ③调 dream 复核(本任务只做 ①②; ③由 Task 6 集成)。

**Files:**
- Modify: `crates/mnemush/src/capacity.rs`(驱逐函数)
- Modify: `crates/mnemush/src/bin/cli.rs`(add 后检查)
- Modify: `crates/mnemush/src/memory.rs`(暴露库大小查询, 或在 capacity.rs 直接用 store.conn)
- Test: `crates/mnemush/src/capacity.rs`

**Interfaces:**
- Consumes: Task 1 `Config::capacity.max_db_mb`; Task 2 `degrade_to_entry`
- Produces: `pub fn db_size_mb(store: &Store) -> Result<f64>`(page_count × page_size);`pub fn eviction_score(m: &Memory) -> f32`;`pub fn evict_wiki_indexes(api) -> Result<usize>`;`pub fn enforce_capacity(api) -> Result<CapacityReport>`
- `CapacityReport { db_mb: f64, limit_mb: f64, evicted_wiki: usize, evicted_low: usize, degraded: usize }`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn db_size_mb_reads_page_count() {
    let (store, _cfg) = test_store();
    let mb = db_size_mb(&store).unwrap();
    assert!(mb > 0.0, "in-memory db still reports size");
}

#[test]
fn eviction_score_prefers_low_value_high_cost() {
    let mut low = NewMemory::note("x", "low");
    low.importance = 0.1;
    let m_low = sample(&low);
    let mut high = NewMemory::note("x", "high");
    high.importance = 0.9;
    let m_high = sample(&high);
    assert!(eviction_score(&m_low) < eviction_score(&m_high), "low importance evicted first");
}

#[test]
fn evict_wiki_indexes_soft_deletes_project() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    api.add(NewMemory::note("w1", "wiki1")).unwrap();
    // 把记忆标为 wiki project(直接 SQL 更新)
    api.store.conn.execute("UPDATE memory SET project='external-wiki'", []).unwrap();
    let n = evict_wiki_indexes(&api).unwrap();
    assert!(n >= 1, "wiki indexes evicted");
    let all = api.list_in_project(100, None).unwrap();
    assert!(all.is_empty(), "all wiki project memories soft-deleted");
}
```

`sample(&NewMemory) -> Memory`: 直接 `Memory` 构造太啰嗦, 用 helper 从 `MemoryApi::add` 拿? 测试里可以建 api + add 然后 get。改写: 用真实 add + get 构造 Memory 再调 eviction_score。

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release capacity::tests`
Expected: FAIL

- [ ] **Step 3: 实现**

```rust
/// 库物理大小 MB(SQLite page_count × page_size)。
pub fn db_size_mb(store: &Store) -> Result<f64> {
    let pages: i64 = store.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let size: i64 = store.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(pages as f64 * size as f64 / 1e6)
}

/// 驱逐评分: 价值/成本。低分先驱逐。
/// score = (importance × confidence × 1/(1+age_days)) / (vec_bytes + content_bytes + edges*64)
pub fn eviction_score(m: &crate::schema::Memory) -> f32 {
    let age_days = (crate::store::Store::now_ts() - m.created_at.timestamp()) as f32 / 86400.0;
    let value = m.importance * m.confidence * (1.0 / (1.0 + age_days.max(0.0)));
    let cost = m.content.len() as f32 + 1024.0 /* vec ~KB */ + (m.tags.len() * 32) as f32;
    value / cost
}

/// ① 清 wiki 临时索引(可再生): 软删 project='external-wiki' 的记忆。
pub fn evict_wiki_indexes(api: &MemoryApi) -> Result<usize> {
    let ids: Vec<String> = api.store.conn
        .prepare("SELECT id FROM memory WHERE deleted_at IS NULL AND project='external-wiki'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in &ids {
        api.soft_delete(id)?;
    }
    Ok(ids.len())
}

/// ② 低分 agent 记忆软删(评分低者先驱逐, 一批)。
pub fn evict_low_value(api: &MemoryApi, batch: usize) -> Result<usize> {
    let all = api.list_in_project(batch * 4, None)?; // 宽松取, 排序后截
    let mut scored: Vec<_> = all.iter().map(|m| (eviction_score(m), m.id.clone())).collect();
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut evicted = 0;
    for (_, id) in scored.iter().take(batch) {
        let Some(m) = api.get(id)? else { continue };
        if m.importance >= 0.7 || m.never_prune || m.memory_type == crate::schema::MemoryType::Identity {
            continue; // 保护规则
        }
        api.soft_delete(id)?;
        evicted += 1;
    }
    Ok(evicted)
}

/// 容量报告(供 status/日志)。
#[derive(Default)]
pub struct CapacityReport {
    pub db_mb: f64,
    pub limit_mb: f64,
    pub evicted_wiki: usize,
    pub evicted_low: usize,
    pub degraded: usize,
}

/// add 后触发: 超限 → ①清 wiki 索引 → ②仍超 → 低分软删。
pub fn enforce_capacity(api: &MemoryApi) -> Result<CapacityReport> {
    let limit = api.config.capacity.max_db_mb;
    let mut rep = CapacityReport { db_mb: db_size_mb(&api.store)?, limit_mb: limit, ..Default::default() };
    if rep.db_mb <= limit {
        return Ok(rep);
    }
    rep.evicted_wiki = evict_wiki_indexes(api)?;
    rep.db_mb = db_size_mb(&api.store)?;
    if rep.db_mb > limit {
        rep.evicted_low = evict_low_value(api, api.config.capacity.eviction_batch)?;
        rep.db_mb = db_size_mb(&api.store)?;
    }
    Ok(rep)
}
```

`sample` helper 测试里改为真实 api:

```rust
fn mk(api: &MemoryApi, title: &str, imp: f32) -> crate::schema::Memory {
    let mut nm = NewMemory::note("content", title);
    nm.importance = imp;
    let id = api.add(nm).unwrap().id;
    api.get(&id).unwrap().unwrap()
}
```

- [ ] **Step 4: add 后触发(cli.rs)**

`Cmd::Add` 分支 `api.add(nm)?` 之后加:

```rust
if let Ok(rep) = mnemush::capacity::enforce_capacity(&api) {
    if rep.evicted_wiki + rep.evicted_low > 0 {
        println!("容量驱逐: 已清 wiki 索引 {} 条, 低分记忆 {} 条 (库 {:.0}/{:.0} MB)", rep.evicted_wiki, rep.evicted_low, rep.db_mb, rep.limit_mb);
    }
}
```

(定位:`cli.rs` 的 `Cmd::Add` 分支, 打印 added 之后。)

- [ ] **Step 5: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add crates/mnemush/src/capacity.rs crates/mnemush/src/bin/cli.rs
git commit -m "✨ capacity: 驱逐链(清wiki索引→低分软删) + add 触发 + 容量报告"
```

---

### Task 4: neuropil 化 —— 规则初筛 + Neuropilize 动作 + 执行

dream 中 LLM 复核后输出 `neuropilize` 动作: 导出到文件树 + 主库降级为摘要入口。

**Files:**
- Modify: `crates/mnemush/src/consolidate.rs`(Action::Neuropilize + 执行 + prompt)
- Modify: `crates/mnemush/src/capacity.rs`(规则初筛 `neuropilize_candidates`)
- Test: `crates/mnemush/src/capacity.rs` + `consolidate.rs`

**Interfaces:**
- Consumes: Task 2 `degrade_to_entry`;`neuropils::export_tree`(已有, 按 project 导出)
- Produces: `pub fn neuropilize_candidates(api, limit) -> Result<Vec<Memory>>`(category ∈ {note, skill} 且 content 非空且非摘要入口);`Action::Neuropilize { id: String, path: String }`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn neuropilize_candidates_filters_by_category() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    api.add(NewMemory::note("concept definition text here", "概念")).unwrap();
    let mut d = NewMemory::note("decision text", "决策");
    d.category = crate::schema::Category::Decision;
    api.add(d).unwrap();
    let cands = neuropilize_candidates(&api, 100).unwrap();
    assert!(cands.iter().any(|m| m.title == "概念"));
    assert!(!cands.iter().any(|m| m.title == "决策"), "decision excluded");
}
```

consolidate.rs 执行测试:

```rust
#[test]
fn neuropilize_action_degrades_to_entry() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let id = add(&api, "npme", 0.3); // 已有 helper: 拨旧 30 天
    let s = execute(&api, &[Action::Neuropilize {
        id: id.clone(),
        path: "out/概念.md".into(),
    }]).unwrap();
    assert_eq!(s.neuropilized, 1);
    let m = api.get(&id).unwrap().unwrap();
    assert!(m.content.len() < "content of npme".len() + 2, "content shrunk to summary");
    assert_eq!(m.context.as_deref(), Some("neuropil:out/概念.md"));
}
```

`ExecStats` 加字段 `neuropilized: usize`(consolidate.rs)。

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release neuropilize`
Expected: FAIL

- [ ] **Step 3: 实现 —— 规则初筛(capacity.rs)**

```rust
/// neuropil 化候选: category ∈ {note, skill}, content 非空, 且非已归档(无 neuropil: context)。
pub fn neuropilize_candidates(api: &MemoryApi, limit: usize) -> Result<Vec<crate::schema::Memory>> {
    let all = api.list_in_project(100000, None)?;
    Ok(all.into_iter()
        .filter(|m| m.deleted_at.is_none())
        .filter(|m| matches!(m.category, crate::schema::Category::Note | crate::schema::Category::Skill))
        .filter(|m| !m.content.is_empty())
        .filter(|m| !m.context.as_deref().map_or(false, |c| c.starts_with("neuropil:")))
        .take(limit)
        .collect())
}
```

- [ ] **Step 4: 实现 —— Action::Neuropilize(consolidate.rs)**

enum 加变体 + parse("neuropilize" type, 字段 `id` + `path`)+ action_order(6, 最后)+ ExecStats.neuropilized + run_one 执行:

```rust
Action::Neuropilize { id, path } => {
    let Some(full) = resolve_id(api, id) else { return Ok(()); };
    if let Some(m) = api.get(&full)? {
        if m.importance >= 0.7 || m.never_prune {
            return Ok(()); // 重要记忆不归档
        }
        crate::capacity::degrade_to_entry(api, &full, path)?;
        stats.neuropilized += 1;
    }
}
```

action_order 返回 6(forget=5 之后)。execute 循环 `0..6` 改 `0..7`。

prompt 加类型说明(consolidate.rs build_prompt):

```
neuropilize({id,path}) 将可结构化记忆归档到文件树(主库留摘要入口), 仅限 category=note/skill 且非重要记忆。
```

- [ ] **Step 5: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add crates/mnemush/src/consolidate.rs crates/mnemush/src/capacity.rs
git commit -m "✨ consolidate: neuropilize 动作(规则初筛+LLM复核+摘要入口降级)"
```

---

### Task 5: neuropil 压缩 —— 冷判定 + 合并 + tar.gz 打包

30 天双条件(入口无命中 + 文件未改)的 neuropil 合并归档页 + 打包移出。

**Files:**
- Modify: `crates/mnemush/src/capacity.rs`(冷判定 + 压缩)
- Test: `crates/mnemush/src/capacity.rs`

**Interfaces:**
- Consumes: Task 1 `Config::capacity.cold_days`; `Memory.context` 里的 neuropil 路径
- Produces: `pub fn is_cold(api, m: &Memory, dir: &Path) -> bool`;`pub fn compress_neuropil(api, neuropils_dir: &Path) -> Result<CompressStats>`
- `CompressStats { merged: usize, archived: usize, freed_mb: f64 }`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn cold_requires_both_conditions() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    let tmp = std::env::temp_dir().join(format!("np-cold-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let f = tmp.join("old.md");
    std::fs::write(&f, "x").unwrap();
    // 文件 40 天前修改
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86400);
    let _ = filetime_set(&f, old); // 用 std::fs::FileTimes? Rust 1.75+: std::fs::File::set_times
    let mut m = NewMemory::note("c", "cold");
    let id = api.add(m.clone()).unwrap().id;
    let mem = api.get(&id).unwrap().unwrap();
    // 入口 40 天前访问
    api.store.conn.execute("UPDATE memory SET last_accessed_at=?1 WHERE id=?2", rusqlite::params![crate::store::Store::now_ts() - 40*86400, id]).unwrap();
    let mem = api.get(&id).unwrap().unwrap();
    assert!(is_cold(&api, &mem, &tmp), "cold when both stale");
    // 文件新鲜 → 不冷
    std::fs::write(&f, "fresh").unwrap();
    assert!(!is_cold(&api, &mem, &tmp), "fresh file not cold");
    std::fs::remove_dir_all(&tmp).unwrap();
}
```

(文件 mtime 设置:Rust 标准库 `std::fs::File::set_times(FileTimes::new().modified(...))` 即可,无需 filetime crate。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release capacity::tests`
Expected: FAIL

- [ ] **Step 3: 实现**

```rust
use std::io::Write;

/// 冷判定: 入口 last_accessed_at > cold_days 且 文件 mtime 未改 > cold_days。
pub fn is_cold(api: &MemoryApi, m: &crate::schema::Memory, neuropils_dir: &Path) -> bool {
    let cold_days = api.config.capacity.cold_days;
    let cutoff = crate::store::Store::now_ts() - cold_days * 86400;
    if m.last_accessed_at.timestamp() > cutoff {
        return false; // 入口近期命中过
    }
    // 文件 mtime
    let Some(path) = neuropil_path(m) else { return false };
    let full = neuropils_dir.join(path);
    match std::fs::metadata(&full) {
        Ok(md) => {
            if let Ok(mt) = md.modified() {
                let mt_ts = mt.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                mt_ts <= cutoff
            } else {
                false
            }
        }
        Err(_) => false, // 文件不存在 → 不判冷(避免误归档)
    }
}

fn neuropil_path(m: &crate::schema::Memory) -> Option<&str> {
    m.context.as_deref()?.strip_prefix("neuropil:")
}

/// 压缩: 冷入口 → 合并归档页 + 打包 tar.gz 移出活动区。
pub fn compress_neuropil(api: &MemoryApi, neuropils_dir: &Path) -> Result<CompressStats> {
    let mut stats = CompressStats::default();
    let all = api.list_in_project(100000, None)?;
    let cold: Vec<crate::schema::Memory> = all.into_iter().filter(|m| is_cold(api, m, neuropils_dir)).collect();
    if cold.is_empty() {
        return Ok(stats);
    }
    let archive_dir = neuropils_dir.join("archive");
    std::fs::create_dir_all(&archive_dir)?;
    // 1) 合并归档页(每 project 一个归档 md)
    let mut per_project: std::collections::BTreeMap<String, Vec<&crate::schema::Memory>> = Default::default();
    for m in &cold {
        let p = m.project.clone().unwrap_or_else(|| "misc".into());
        per_project.entry(p).or_default().push(m);
    }
    for (proj, mems) in per_project {
        let mut page = format!("---\ntitle: archived-{proj}\ncategory: note\n---\n\n# 归档 {proj}({} 条)\n\n", mems.len());
        for m in mems {
            page.push_str(&format!("## {}\n\n源: `{}`\n\n{}\n\n", m.title, neuropil_path(m).unwrap_or(""), m.content));
        }
        let fname = archive_dir.join(format!("{proj}.md"));
        std::fs::write(&fname, page)?;
        stats.merged += mems.len();
    }
    // 2) tar.gz 打包 archive 目录 → archive.tar.gz, 删除原目录
    let tar_path = archive_dir.with_extension("tar.gz");
    let tar_gz = std::fs::File::create(&tar_path)?;
    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::Default);
    let mut tar = tar::Builder::new(enc);
    // 只打包归档 md(不含 .tar.gz 自身)
    for entry in std::fs::read_dir(&archive_dir)? {
        let e = entry?;
        if e.path().extension().map_or(false, |x| x == "md") {
            tar.append_path_with_name(&e.path(), e.file_name().to_string_lossy().as_ref())?;
        }
    }
    tar.finish()?;
    let gz = tar.into_inner()?;
    gz.finish()?;
    // 删除 md(保留 tar.gz)
    for entry in std::fs::read_dir(&archive_dir)? {
        let e = entry?;
        if e.path().extension().map_or(false, |x| x == "md") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    stats.archived = cold.len();
    stats.freed_mb = 0.0; // 文件树空间不计入库 MB
    Ok(stats)
}

#[derive(Default)]
pub struct CompressStats {
    pub merged: usize,
    pub archived: usize,
    pub freed_mb: f64,
}
```

- [ ] **Step 4: 运行测试**

Run: `cd crates/mnemush && cargo test --release capacity::tests`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/capacity.rs
git commit -m "✨ capacity: neuropil 压缩(双条件冷判定 + 合并归档 + tar.gz 打包)"
```

---

### Task 6: dream 三合一集成 + status 容量段

dream 流程: 遗忘 + neuropilize 复核 + 压缩 + 容量报告; `mnemush status` 加容量段。

**Files:**
- Modify: `crates/mnemush/src/consolidate.rs`(run_consolidate 加 dream 扩展: 初筛候选并入 prompt + 压缩 + 报告)
- Modify: `crates/mnemush/src/bin/cli.rs`(status 容量段 + dream 输出容量行)
- Test: `crates/mnemush/src/consolidate.rs`(集成逻辑单测, 不调真实 LLM)

**Interfaces:**
- Consumes: Task 3 `enforce_capacity`/`db_size_mb`; Task 4 `neuropilize_candidates`; Task 5 `compress_neuropil`
- Produces: dream 结束时打印: `dream: N candidates | ... | 容量 X/100 MB, wiki索引 Y, 归档 Z`

- [ ] **Step 1: 写测试(初筛候选并入 dream 候选池)**

```rust
#[test]
fn dream_includes_neuropilize_candidates_in_prompt() {
    let (store, cfg) = test_store();
    let api = MemoryApi::new(&store, &cfg);
    api.add(NewMemory::note("concept body for np", "概念甲")).unwrap();
    let cands = crate::capacity::neuropilize_candidates(&api, 10).unwrap();
    assert!(!cands.is_empty(), "note candidate surfaced");
    let prompt = build_prompt(&cands, true);
    assert!(prompt.iter().any(|m| m.content.contains("概念甲")), "candidate in prompt");
}
```

(逻辑测试: 初筛候选进入 dream prompt。真实 LLM 集成在端到端手动验证。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release dream_includes`
Expected: PASS(此测试验证已存在的 build_prompt 行为 + 初筛; 若已绿, 跳过失败步骤, 直接进入实现后验证)

- [ ] **Step 3: 实现 —— run_consolidate dream 扩展**

`run_consolidate` 中, dream 分支在 collect 后合并初筛候选(去重):

```rust
let mut cands = collect_candidates(api, opts.project.as_deref(), since_ts)?;
cands.truncate(5);
if opts.dream {
    // neuropil 化初筛候选并入(规则初筛 → LLM 复核输出 neuropilize)
    let np = crate::capacity::neuropilize_candidates(api, 5)?;
    for m in np {
        if !cands.iter().any(|c| c.id == m.id) {
            cands.push(m);
        }
    }
    cands.truncate(5);
}
```

(初筛候选与遗忘候选共用 5 条批次; 压缩与容量报告在 dream 尾部, 不占用 LLM 批次 —— 纯文件/DB 操作。)

dream 尾部(execute 之后):

```rust
if opts.dream {
    // neuropil 压缩(冷归档) — 文件树操作, 不占 LLM 批次
    let np_dir = crate::default_data_dir().join("neuropils");
    if let Ok(cs) = crate::capacity::compress_neuropil(&api, &np_dir) {
        if cs.archived > 0 {
            println!("neuropil 压缩: 归档 {} 条 (合并 {} 页)", cs.archived, cs.merged);
        }
    }
    // 容量报告
    if let Ok(rep) = crate::capacity::enforce_capacity(&api) {
        println!("容量: {:.0}/{:.0} MB", rep.db_mb, rep.limit_mb);
    }
}
```

注意:`default_data_dir()/neuropils` 是 import-tree 默认目录(neuropils.rs 用 `~/.mnemush/neuropils/`),确认常量一致 —— 检查 neuropils.rs 的默认路径常量并复用。

- [ ] **Step 4: status 容量段(cli.rs)**

`Cmd::Status` 输出后加:

```rust
let size = mnemush::capacity::db_size_mb(&store).unwrap_or(0.0);
let limit = config.capacity.max_db_mb;
let np_count: i64 = store.conn.query_row(
    "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL AND context LIKE 'neuropil:%'",
    [], |r| r.get(0),
)?;
println!("  capacity:    {size:.0}/{limit:.0} MB (neuropil 入口 {np_count})");
```

- [ ] **Step 5: 运行全量测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿

- [ ] **Step 6: build + codesign + 端到端**

```bash
cd crates/mnemush && cargo build --release && cp target/release/mnemush target/release/mnemush-mcp ~/.cargo/bin/ && codesign --force --sign - ~/.cargo/bin/mnemush ~/.cargo/bin/mnemush-mcp
```

临时库端到端: 建临时库 → add 几条低 importance 旧记忆 + 一条概念 note → `--db <tmp> dream --dry-run` 看 LLM 输出含 neuropilize → `--db <tmp> dream` 执行 → status 显示容量段 → 检查摘要入口(context=neuropil:)。

- [ ] **Step 7: 更新 CHANGELOG + README**

CHANGELOG v1.3.0: 容量管理(100MB 硬阈值/驱逐链/neuropil 化/压缩)+ dream 三合一。README Status 段补 v1.3。

- [ ] **Step 8: 提交**

```bash
git add crates/mnemush/src/consolidate.rs crates/mnemush/src/bin/cli.rs CHANGELOG.md README.md
git commit -m "✨ dream: 三合一(遗忘+neuropil化+压缩) + status 容量段"
```

---

## Self-Review

**Spec 覆盖:**
- 物理 ≤100MB 硬阈值 → Task 1(配置)+ Task 3(驱逐链 + add 触发)✓
- 条数交 LLM 遗忘 → 既有 consolidate/dream 遗忘 + Task 4(neuropilize 减少常驻)✓
- wiki 动态索引清理 → Task 3 evict_wiki_indexes ✓
- neuropil 化(规则初筛 + LLM 复核 + 摘要入口)→ Task 2 + Task 4 ✓
- neuropil 压缩(双条件冷判定 + 合并 + 打包)→ Task 5 ✓
- dream 三合一 + 容量报告 → Task 6 ✓
- 监控(status 容量段)→ Task 6 ✓
- 摘要截取规则(前 2 句)→ Task 2 ✓
- 零 schema 改动(path 存 context)→ Task 2 ✓
- 错误处理(驱逐/归档失败不阻塞)→ 各任务用 `if let Ok(...)` / 逐条 continue ✓

**占位符:** 无 TBD/TODO; 每步含真实代码。

**类型一致性:** `CapacityConfig`(Task 1)→ `api.config.capacity`(Task 2/3/5)✓;`degrade_to_entry`(Task 2)→ `Action::Neuropilize` 执行(Task 4)✓;`eviction_score`/`enforce_capacity`(Task 3)→ status/dream(Task 6)✓;`compress_neuropil`(Task 5)→ dream(Task 6)✓。`ExecStats.neuropilized` 由 Task 4 加入, Task 6 的 dream 输出行引用它 —— Task 4 必须先于 Task 6。
