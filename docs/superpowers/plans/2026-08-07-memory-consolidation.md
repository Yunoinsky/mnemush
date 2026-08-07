# 记忆巩固 consolidate(增量整合 + 主动遗忘) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `mnemush consolidate` —— LLM 驱动的增量记忆整合 + 主动遗忘(Karpathy wiki 编译 + 钟毅团队主动遗忘映射)。`dream`(全量)留后续。

**Architecture:** 新增 `llm.rs`(MiniMax M3 → DeepSeek V4 Flash fallback 聊天客户端)与 `consolidate.rs`(候选收集/prompt/动作解析/执行器/位置记录)。CLI 挂 `consolidate` 命令。复用 `MemoryApi::update/add/soft_delete`、`EdgeApi::link`、auto-merge 逻辑。

**Tech Stack:** Rust(crates/mnemush)、ureq(已有,embedding 在用)、serde_json、regex。无新依赖。

**Spec:** `docs/superpowers/specs/2026-08-07-memory-consolidation-design.md`

## Global Constraints

- 无新 crate 依赖(ureq 已有)
- 现有命令/MCP 零改动
- LLM:MiniMax `api.minimax.chat/v1/text/chatcompletion_v2` model `minimax-m3`(可配置);fallback DeepSeek `https://api.deepseek.com/chat/completions` model `deepseek-v4-flash`(可配置);60s 超时;两者都失败 → 报错退出
- 保护规则: importance≥0.7 / never_prune / identity / 最近 7 天创建 → 禁止 forget/decay
- 位置记录 `~/.mnemush/consolidate.json`(last_ts);增量取 `created_at > last_ts`
- 动作执行顺序: link → update → merge → decay → forget
- 构建后 codesign(签名陷阱)
- gitmoji 提交

---

### Task 1: llm.rs — 聊天客户端(MiniMax + DeepSeek fallback)

**Files:**
- Create: `crates/mnemush/src/llm.rs`
- Modify: `crates/mnemush/src/lib.rs`(`pub mod llm;`)
- Test: `llm.rs` 内 `mod tests`(mock TCP server)

**Interfaces:**
- Produces:
  - `pub fn chat(messages: &[ChatMsg]) -> Result<String>` — 尝试 MiniMax(`MINIMAX_API_KEY`,`~/.mmx/config.json` 兜底),失败 fallback DeepSeek(`DEEPSEEK_API_KEY`);返回助手文本
  - `pub struct ChatMsg { pub role: String, pub content: String }`
  - `pub const MINIMAX_CHAT_URL: &str = "https://api.minimax.chat/v1/text/chatcompletion_v2";`
  - `pub const DEEPSEEK_CHAT_URL: &str = "https://api.deepseek.com/chat/completions";`
  - `pub fn minimax_model() -> String`(env `MNEMUSH_LLM_MODEL` 或 "minimax-m3")
  - `pub fn deepseek_model() -> String`(env `MNEMUSH_DEEPSEEK_MODEL` 或 "deepseek-v4-flash")

- [ ] **Step 1: 写失败的 mock 测试**

```rust
// llm.rs 顶部 + tests:
use crate::error::Result;
use ureq::AgentBuilder;
use std::time::Duration;

pub struct ChatMsg { pub role: String, pub content: String }

impl ChatMsg {
    pub fn user(s: &str) -> Self { Self { role: "user".into(), content: s.into() } }
    pub fn system(s: &str) -> Self { Self { role: "system".into(), content: s.into() } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 本地 mock HTTP server: 返回预设 OpenAI 风格 JSON。
    fn mock_server(body: &'static str) -> (TcpListener, u16) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(), body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (l, port)
    }

    #[test]
    fn parses_openai_style_response() {
        let (_l, port) = mock_server(r#"{"choices":[{"message":{"content":"hello from mock"}}]}"#);
        let url = format!("http://127.0.0.1:{port}/v1");
        // 直接调内部解析函数
        let out = crate::llm::parse_chat_response(
            r#"{"choices":[{"message":{"content":"hello from mock"}}]}"#).unwrap();
        assert_eq!(out, "hello from mock");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release llm::tests::parses`
Expected: FAIL(cannot find `parse_chat_response`)

- [ ] **Step 3: 实现 chat 客户端**

```rust
pub fn minimax_model() -> String {
    std::env::var("MNEMUSH_LLM_MODEL").unwrap_or_else(|_| "minimax-m3".into())
}
pub fn deepseek_model() -> String {
    std::env::var("MNEMUSH_DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into())
}

fn minimax_key() -> Option<String> {
    if let Ok(k) = std::env::var("MINIMAX_API_KEY") { return Some(k); }
    let cfg = std::path::Path::new(&crate::default_data_dir()).join("..").join("..");
    // ~/.mmx/config.json
    let mmx = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mmx").join("config.json");
    if mmx.exists() {
        if let Ok(t) = std::fs::read_to_string(&mmx) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                if let Some(k) = v.get("api_key").and_then(|x| x.as_str()) {
                    return Some(k.to_string());
                }
            }
        }
    }
    None
}

fn post_json(url: &str, bearer: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    let agent = AgentBuilder::new().timeout(Duration::from_secs(60)).build();
    let resp = agent
        .post(url)
        .set("Authorization", &format!("Bearer {bearer}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| crate::error::MnemushError::Other(format!("llm http: {e}")))?;
    let text = resp.into_string()
        .map_err(|e| crate::error::MnemushError::Other(format!("llm body: {e}")))?;
    Ok(serde_json::from_str(&text)
        .map_err(|e| crate::error::MnemushError::Other(format!("llm json: {e}")))?)
}

pub fn parse_chat_response(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| crate::error::MnemushError::Other(format!("llm json: {e}")))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| crate::error::MnemushError::Other("llm: no choices/0/message/content".into()))
}

pub fn chat(messages: &[ChatMsg]) -> Result<String> {
    let payload = |model: &str| serde_json::json!({
        "model": model,
        "messages": messages.iter().map(|m| serde_json::json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
        "max_tokens": 4000,
    });
    // 1) MiniMax
    if let Some(key) = minimax_key() {
        if let Ok(v) = post_json(MINIMAX_CHAT_URL, &key, &payload(&minimax_model())) {
            if let Ok(text) = parse_chat_response(&v.to_string()) {
                if !text.trim().is_empty() { return Ok(text); }
            }
        }
    }
    // 2) DeepSeek fallback
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        let v = post_json(DEEPSEEK_CHAT_URL, &key, &payload(&deepseek_model()))?;
        return parse_chat_response(&v.to_string());
    }
    Err(crate::error::MnemushError::Other(
        "llm: no usable key (MINIMAX_API_KEY / DEEPSEEK_API_KEY)".into()))
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release llm`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/llm.rs crates/mnemush/src/lib.rs
git commit -m "✨ llm: MiniMax M3 + DeepSeek fallback 聊天客户端"
```

---

### Task 2: consolidate.rs — 动作解析与执行器

**Files:**
- Create: `crates/mnemush/src/consolidate.rs`
- Modify: `crates/mnemush/src/lib.rs`
- Test: `consolidate.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `llm::chat`、`MemoryApi`、`EdgeApi`
- Produces:
  - `pub enum Action { Update{id, content, reason}, Link{source, target, etype, strength}, Merge{keep, absorb}, Insight{title, content, links}, Decay{id, factor, reason}, Forget{id, reason} }`
  - `pub fn parse_actions(json: &str) -> Result<Vec<Action>>` — 解析 `{"actions":[...]}`,未知 type 跳过
  - `pub struct ExecStats { pub updated: usize, pub links: usize, pub merged: usize, pub insights: usize, pub decayed: usize, pub forgot: usize, pub errors: Vec<String> }`
  - `pub fn execute(api: &MemoryApi, actions: &[Action]) -> Result<ExecStats>` — 顺序 link→update→merge→decay→forget,含保护规则;单条失败记录错误继续
  - `fn is_protected(m: &Memory) -> bool` — importance≥0.7 || never_prune || memory_type==identity || created 7 天内

- [ ] **Step 1: 写失败的测试(保护规则 + 各动作)**

```rust
// consolidate.rs tests:
    use crate::config::Config;
    use crate::memory::MemoryApi;
    use crate::schema::{Category, NewMemory, Source};
    use crate::store::Store;

    fn test_store() -> (Store, Config) { (Store::open_in_memory().unwrap(), Config::default()) }
    fn add(api: &MemoryApi, title: &str, imp: f32) -> String {
        let mut nm = NewMemory::note(format!("content of {title}"), title);
        nm.importance = imp;
        api.add(nm).unwrap().id
    }

    #[test]
    fn parse_actions_skips_unknown_types() {
        let a = parse_actions(r#"{"actions":[{"type":"update","id":"x","content":"c"},{"type":"bogus","id":"y"}]}"#).unwrap();
        assert_eq!(a.len(), 1);
        match &a[0] { Action::Update{..} => {}, _ => panic!("expected update") }
    }

    #[test]
    fn forget_respects_protection() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "protected", 0.9);  // importance ≥ 0.7
        let s = execute(&api, &[Action::Forget { id: id.clone(), reason: "test".into() }]).unwrap();
        assert_eq!(s.forgot, 0, "protected memory not forgotten");
        assert!(api.get(&id).unwrap().is_some());
    }

    #[test]
    fn decay_lowers_confidence_and_respects_floor() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "decayme", 0.3);
        let s = execute(&api, &[Action::Decay { id: id.clone(), factor: 0.5, reason: "stale".into() }]).unwrap();
        assert_eq!(s.decayed, 1);
        let m = api.get(&id).unwrap().unwrap();
        assert!((m.confidence - 0.5).abs() < 1e-6, "confidence halved");
        // 多次 decay 不跌破 0.05 下限
        for _ in 0..10 {
            execute(&api, &[Action::Decay { id: id.clone(), factor: 0.1, reason: "x".into() }]).unwrap();
        }
        let m = api.get(&id).unwrap().unwrap();
        assert!(m.confidence >= 0.05 - 1e-6, "floor respected");
    }

    #[test]
    fn forget_soft_deletes() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "forgetme", 0.3);
        let s = execute(&api, &[Action::Forget { id: id.clone(), reason: "obsolete".into() }]).unwrap();
        assert_eq!(s.forgot, 1);
        assert!(api.get(&id).unwrap().is_none(), "soft-deleted");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release consolidate::tests`
Expected: FAIL(cannot find `parse_actions` / `execute`)

- [ ] **Step 3: 实现**

```rust
use crate::error::Result;
use crate::memory::MemoryApi;
use crate::schema::{ActionStatus, Category, EdgeType, Memory, NewMemory, Source};

pub enum Action {
    Update { id: String, content: String, reason: String },
    Link { source: String, target: String, etype: String, strength: f32 },
    Merge { keep: String, absorb: String },
    Insight { title: String, content: String, links: Vec<String> },
    Decay { id: String, factor: f32, reason: String },
    Forget { id: String, reason: String },
}

pub fn parse_actions(json: &str) -> Result<Vec<Action>> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| crate::error::MnemushError::Other(format!("actions json: {e}")))?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("actions").and_then(|a| a.as_array()) {
        for item in arr {
            let t = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let s = |k: &str| item.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            match t {
                "update" => out.push(Action::Update { id: s("id"), content: s("content"), reason: s("reason") }),
                "link" => out.push(Action::Link {
                    source: s("source"), target: s("target"), etype: s("etype"),
                    strength: item.get("strength").and_then(|x| x.as_f64()).unwrap_or(0.6) as f32,
                }),
                "merge" => out.push(Action::Merge { keep: s("keep"), absorb: s("absorb") }),
                "insight" => out.push(Action::Insight {
                    title: s("title"), content: s("content"),
                    links: item.get("links").and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default(),
                }),
                "decay" => out.push(Action::Decay {
                    id: s("id"),
                    factor: item.get("factor").and_then(|x| x.as_f64()).unwrap_or(0.5) as f32,
                    reason: s("reason"),
                }),
                "forget" => out.push(Action::Forget { id: s("id"), reason: s("reason") }),
                _ => {} // unknown type: skip
            }
        }
    }
    Ok(out)
}

pub struct ExecStats {
    pub updated: usize, pub links: usize, pub merged: usize,
    pub insights: usize, pub decayed: usize, pub forgot: usize,
    pub errors: Vec<String>,
}

fn is_protected(m: &Memory) -> bool {
    if m.never_prune || m.memory_type == crate::schema::MemoryType::Identity { return true; }
    if m.importance >= 0.7 { return true; }
    let week_ago = chrono::Utc::now() - chrono::Duration::days(7);
    m.created_at > week_ago
}

fn run_one(api: &MemoryApi, action: &Action, stats: &mut ExecStats) -> Result<()> {
    let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
    match action {
        Action::Update { id, content, reason } => {
            if let Some(mut m) = api.get(id)? {
                m.content = content.clone();
                m.content_hash = MemoryApi::content_hash(content);
                api.update(&m)?;
                api.store.log_event("consolidate_update", Some(id), None, Some(reason), "consolidate")?;
                stats.updated += 1;
            }
        }
        Action::Link { source, target, etype, strength } => {
            let et = match etype.as_str() {
                "supports" => EdgeType::Supports,
                "contradicts" => EdgeType::Contradicts,
                _ => EdgeType::Related,
            };
            edge_api.link(source, target, et, *strength, Some("consolidate:link"), None)?;
            stats.links += 1;
        }
        Action::Merge { keep, absorb } => {
            let (Some(k), Some(a)) = (api.get(keep)?, api.get(absorb)?) else { return Ok(()); };
            if is_protected(&a) && !is_protected(&k) { return Ok(()); } // 不把保护记忆并入普通
            let mut k = k;
            k.content = format!("{}\n\n---\n\n{}", k.content, a.content);
            k.content_hash = MemoryApi::content_hash(&k.content);
            api.update(&k)?;
            api.soft_delete(absorb)?;
            // 边重定向: absorb 的边指向 keep
            api.store.redirect_edges(absorb, keep)?;
            api.store.log_event("consolidate_merge", Some(absorb), None, Some(&format!("into {keep}")), "consolidate")?;
            stats.merged += 1;
        }
        Action::Insight { title, content, links } => {
            let mut nm = NewMemory::note(content.clone(), title.clone());
            nm.category = Category::Insight;
            nm.source = Source::Consolidate;
            let r = api.add(nm)?;
            let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
            for l in links {
                let _ = edge_api.link(&r.id, l, EdgeType::Related, 0.7, Some("consolidate:insight"), None);
            }
            stats.insights += 1;
        }
        Action::Decay { id, factor, reason } => {
            if let Some(mut m) = api.get(id)? {
                if is_protected(&m) { return Ok(()); }
                m.confidence = (m.confidence * factor).max(0.05);
                api.update(&m)?;
                api.store.log_event("consolidate_decay", Some(id), None, Some(reason), "consolidate")?;
                stats.decayed += 1;
            }
        }
        Action::Forget { id, reason } => {
            if let Some(m) = api.get(id)? {
                if is_protected(&m) { return Ok(()); }
                api.soft_delete(id)?;
                api.store.log_event("consolidate_forget", Some(id), None, Some(reason), "consolidate")?;
                stats.forgot += 1;
            }
        }
    }
    Ok(())
}

pub fn execute(api: &MemoryApi, actions: &[Action]) -> Result<ExecStats> {
    // 顺序: link → update → merge → decay → forget
    let mut stats = ExecStats { updated: 0, links: 0, merged: 0, insights: 0, decayed: 0, forgot: 0, errors: vec![] };
    for order in [0usize, 1, 2, 3, 4, 5] {
        let bucket: Vec<&Action> = actions.iter().filter(|a| action_order(a) == order).collect();
        for a in bucket {
            if let Err(e) = run_one(api, a, &mut stats) {
                stats.errors.push(format!("{e}"));
            }
        }
    }
    Ok(stats)
}

fn action_order(a: &Action) -> usize {
    match a {
        Action::Link{..} => 0, Action::Update{..} => 1, Action::Merge{..} => 2,
        Action::Insight{..} => 3, Action::Decay{..} => 4, Action::Forget{..} => 5,
    }
}
```

> 注意: 上述实现引用了 `api.store.log_event`、`api.store.redirect_edges`、`Source::Consolidate`、`Category::Insight` —— 若这些不存在需补:
> - `Source::Consolidate` 加进 schema.rs + store.rs parse_source(Task 1 已加 FileTree 模式, 照做)
> - `Category::Insight` 检查是否已有(如无, 加)
> - `redirect_edges` 检查 store 是否有(如无, 用 `UPDATE memory_edge SET ...` 实现)
> - `log_event` 确认签名(store.log_event(event, memory_id, ?, reason, actor))

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release consolidate`
Expected: PASS(4 测试)

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/consolidate.rs crates/mnemush/src/schema.rs crates/mnemush/src/store.rs crates/mnemush/src/lib.rs
git commit -m "✨ consolidate: 动作解析 + 执行器(保护规则/双阈值/审计)"
```

---

### Task 3: consolidate 命令(候选收集 + prompt + 位置)

**Files:**
- Modify: `crates/mnemush/src/bin/cli.rs`
- Modify: `crates/mnemush/src/consolidate.rs`(候选收集 + prompt + run)
- Test: `consolidate.rs` tests(mock LLM 端到端)

**Interfaces:**
- Consumes: `llm::chat`、`parse_actions`、`execute`
- Produces:
  - `pub struct CState { pub last_ts: i64 }` — 位置记录
  - `pub fn load_state() -> CState` / `pub fn save_state(&CState) -> Result<()>`(`~/.mnemush/consolidate.json`)
  - `pub fn collect_candidates(api: &MemoryApi, project: Option<&str>, since: Option<i64>) -> Result<Vec<Memory>>`
  - `pub fn build_prompt(cands: &[Memory], now: &str, is_dream: bool) -> Vec<llm::ChatMsg>`
  - `pub fn run_consolidate(api: &MemoryApi, opts: &RunOpts) -> Result<(ExecStats, usize)>` — 全流程: collect → prompt → chat → parse → execute → save_state
  - `pub struct RunOpts { pub project: Option<String>, pub dry_run: bool, pub suggest: bool, pub since: Option<i64> }`

- [ ] **Step 1: 写失败的测试(mock LLM 端到端: 位置更新 + 动作执行)**

```rust
// consolidate.rs tests 追加:
    #[test]
    fn run_consolidate_executes_llm_actions_and_updates_state() {
        // 用一个固定响应, 通过注入 chat 实现(测试用 env 开关指向 mock)
        // 简化: 直接测 collect+execute 组合
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "old", 0.3);
        let actions = parse_actions(r#"{"actions":[{"type":"decay","id":"__ID__","factor":0.5,"reason":"stale"}]}"#.replace("__ID__", &id)).unwrap();
        let s = execute(&api, &actions).unwrap();
        assert_eq!(s.decayed, 1);
        let m = api.get(&id).unwrap().unwrap();
        assert!((m.confidence - 0.5).abs() < 1e-6);
        // 位置状态往返
        let st = CState { last_ts: 12345 };
        save_state(&st).unwrap();
        let st2 = load_state();
        assert_eq!(st2.last_ts, 12345);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release consolidate::tests::run_consolidate`
Expected: FAIL(cannot find `CState`)

- [ ] **Step 3: 实现候选收集/prompt/run**

```rust
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct CState { pub last_ts: i64 }

fn state_path() -> PathBuf { crate::default_data_dir().join("consolidate.json") }

pub fn load_state() -> CState {
    let p = state_path();
    if let Ok(t) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if let Some(ts) = v.get("last_ts").and_then(|x| x.as_i64()) {
                return CState { last_ts: ts };
            }
        }
    }
    CState::default()
}

pub fn save_state(s: &CState) -> Result<()> {
    std::fs::create_dir_all(crate::default_data_dir())?;
    std::fs::write(state_path(), serde_json::json!({"last_ts": s.last_ts}).to_string())?;
    Ok(())
}

pub fn collect_candidates(api: &MemoryApi, project: Option<&str>, since: Option<i64>) -> Result<Vec<crate::schema::Memory>> {
    let all = api.list_in_project(100000, project)?;
    let since_ts = since.unwrap_or(0);
    Ok(all.into_iter()
        .filter(|m| m.deleted_at.is_none() && m.created_at.timestamp() > since_ts)
        .collect())
}

fn trunc(s: &str, n: usize) -> String {
    let mut out = s.chars().take(n).collect::<String>();
    if s.chars().count() > n { out.push('…'); }
    out
}

pub fn build_prompt(cands: &[crate::schema::Memory], is_dream: bool) -> Vec<llm::ChatMsg> {
    let mut items = String::new();
    for (i, m) in cands.iter().enumerate() {
        items.push_str(&format!(
            "[{}] id={} category={} importance={:.2} confidence={:.2} created={}\ntitle: {}\ncontent: {}\n---\n",
            i, &m.id[..8], m.category.as_str(), m.importance, m.confidence, m.created_at.date_naive(),
            m.title, trunc(&m.content, 400),
        ));
    }
    let sys = format!(
        "你是记忆库巩固者。分析以下候选记忆,输出 JSON 动作列表。\n\
         巩固: update(修订内容)/ link(建边)/ merge(合并重复)/ insight(发现跨簇新模式, 创建顿悟记忆)。\n\
         主动遗忘: decay(降权, 原因: 干扰|过时|冗余)/ forget(软删, 原因: 过时|冗余|被取代|干扰)。\n\
         双阈值: confidence<0.4 的记忆低证据即可遗忘; confidence≥0.4 需明确矛盾/过时证据。\n\
         保护规则: importance≥0.7 / never_prune / identity / 7 天内创建 → 禁止 decay/forget, 只能 update 或标 contradicts。\n\
         输出严格 JSON: {{\"actions\":[{{\"type\":\"...\",\"id\":\"<前8字符id>\",...}}]}}, 不要其它文字。\n\
         遗忘强度: {}\n\n候选记忆:\n{}",
        if is_dream { "高(睡眠期巩固高峰, 可更激进)" } else { "中" },
        items,
    );
    vec![llm::ChatMsg::system(&sys), llm::ChatMsg::user("请分析并输出动作。")]
}

pub struct RunOpts {
    pub project: Option<String>,
    pub dry_run: bool,
    pub suggest: bool,
    pub since: Option<i64>,
}

pub fn run_consolidate(api: &MemoryApi, opts: &RunOpts) -> Result<(ExecStats, usize)> {
    let since_ts = opts.since.or_else(|| {
        let st = load_state();
        if st.last_ts > 0 { Some(st.last_ts) } else { None }
    });
    let cands = collect_candidates(api, opts.project.as_deref(), since_ts)?;
    if cands.is_empty() { return Ok((ExecStats { ..Default::default() }, 0)); }
    let prompt = build_prompt(&cands, false);
    let raw = llm::chat(&prompt)?;
    // 存档原始响应
    let _ = std::fs::create_dir_all(crate::default_data_dir().join("eval"));
    let _ = std::fs::write(
        crate::default_data_dir().join("eval").join(format!("consolidate-{}.json", chrono::Utc::now().timestamp())),
        raw.clone());
    let actions = parse_actions(&raw)?;
    if opts.suggest {
        println!("{}", raw);
        return Ok((ExecStats::default(), cands.len()));
    }
    let stats = if opts.dry_run {
        for a in &actions { println!("{:?}", a); }
        ExecStats::default()
    } else {
        execute(api, &actions)?
    };
    // 更新位置: 到最新记忆的 created_at
    let max_ts = cands.iter().map(|m| m.created_at.timestamp()).max().unwrap_or(0);
    save_state(&CState { last_ts: max_ts })?;
    Ok((stats, cands.len()))
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release consolidate`
Expected: PASS

- [ ] **Step 5: CLI 挂 `consolidate` 命令**

```rust
// cli.rs, Cmd enum(ExportTree 后):
    /// Consolidate memories: LLM-driven integration + active forgetting.
    Consolidate {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        suggest: bool,
        #[arg(long)]
        since: Option<i64>,
    },
// match:
        Cmd::Consolidate { project, dry_run, suggest, since } => {
            let api = MemoryApi::new(&store, &config);
            let opts = mnemush::consolidate::RunOpts {
                project, dry_run, suggest, since,
            };
            let (s, n) = mnemush::consolidate::run_consolidate(&api, &opts)?;
            if !dry_run && !suggest {
                println!(
                    "consolidate: {} candidates | +{} updated, +{} links, +{} merged, +{} insight, -{} decayed, -{} forgot | {} error(s)",
                    n, s.updated, s.links, s.merged, s.insights, s.decayed, s.forgot, s.errors.len());
                for e in &s.errors { println!("  ⚠ {e}"); }
            }
        }
```

- [ ] **Step 6: 全量测试 + 安装 + 重签 + 端到端**

Run: `cargo test --release`(全部 PASS)

```bash
cp crates/mnemush/target/release/mnemush crates/mnemush/target/release/mnemush-mcp ~/.cargo/bin/
codesign --force --sign - ~/.cargo/bin/mnemush ~/.cargo/bin/mnemush-mcp
```

```bash
# 端到端: 建几条记忆 → consolidate --suggest 看 LLM 输出 → consolidate 执行
mnemush add "Stale note" "old info about xyz that is outdated" -c note
mnemush add "Clash proxy reminder" "clash listens on 7890" -c note
mnemush consolidate --suggest   # 预览 LLM 建议
mnemush consolidate            # 执行
mnemush status
```

Expected: consolidate 报告 +N candidates, 动作数;重复跑 0 candidates(位置已更新)。

- [ ] **Step 7: CHANGELOG + 提交**

```bash
git add crates/mnemush/src/consolidate.rs crates/mnemush/src/bin/cli.rs CHANGELOG.md
git commit -m "✨ consolidate: LLM 驱动记忆整合 + 主动遗忘(双阈值/保护/位置记录)"
```

---

## Self-Review 记录

- **Spec 覆盖**: consolidate 增量(Task 3)✓ / 遗忘评估(双阈值+保护, Task 2)✓ / 六类动作(Task 2)✓ / 审计 memory_event + JSON 存档(Task 2/3)✓ / fallback(Task 1)✓ / 位置记录(Task 3)✓ / --dry-run/--suggest(Task 3)✓ / dream=非目标(留后续)
- **类型一致**: `Action`/`ExecStats`/`RunOpts`/`CState` 前后一致;`llm::chat(&[ChatMsg]) -> Result<String>` Task 1 定义, Task 3 消费
- **需在实现时确认**: `Source::Consolidate` / `Category::Insight` 是否存在(schema.rs);`store.log_event` / `store.redirect_edges` 签名;如缺则补(Task 2 已注明)
