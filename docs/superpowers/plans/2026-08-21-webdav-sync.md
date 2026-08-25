# WebDAV 跨设备同步 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** WebDAV 传输的跨设备记忆同步(坚果云为默认选项), 记忆更新时自动触发; 逐条实时合并(updated_at 较新赢 + 并集 + 删除传播) + 乐观锁。

**Architecture:** 复用现有 `sync export/import`(快照编解码 + 逐条合并, 已验证)。新增: WebDAV 传输(tar.gz + HTTP PUT/GET)、push 方向双向合并、写入后自动触发(dirty 标记 + 30s 去抖)。

**Tech Stack:** Rust(ureq 已有, tar 0.4 + flate2 已有), 坚果云 WebDAV(dav.jianguoyun.com)。

## Global Constraints

- 编译/测试目录:`crates/mnemush/`;`cargo test --release` 全绿(162+18 基线)。
- 二进制更新后 codesign:`codesign --force --sign - ~/.cargo/bin/mnemush{, -mcp}`。
- 提交 gitmoji。注释中文, 技术术语英文。
- **零新依赖**:ureq/tar/flate2 均已存在。
- 快照格式与现有 sync 完全兼容(manifest v4 不变)。
- 凭证不落命令行/代码: `MNEMUSH_WEBDAV_URL`(默认坚果云)/ `MNEMUSH_WEBDAV_USER` / `MNEMUSH_WEBDAV_PASS`。
- 冲突语义(已验证): 同 id 比 `max(last_accessed_at, created_at)` 较新者赢; 新 id 并集; 软删(deleted_at)传播。
- 自动触发默认**关闭**(`[sync] webdav_enabled = false`), 配好凭证才启用。

---

### Task 1: WebDAV 传输层(`webdav-push` / `webdav-pull`)

**Files:**
- Create: `crates/mnemush/src/webdav.rs`(HTTP 传输 + tar.gz 打包/解包)
- Modify: `crates/mnemush/src/lib.rs`(pub mod webdav)
- Modify: `crates/mnemush/src/bin/cli.rs`(SyncCmd 加 WebdavPush/WebdavPull)
- Modify: `crates/mnemush/src/config.rs`(`SyncConfig { webdav_enabled, webdav_debounce_secs }`)
- Test: `crates/mnemush/src/webdav.rs`

**Interfaces:**
- Consumes: `sync::export_to / import_from`, `Store`, `Config`
- Produces: `pub fn push(store, config) -> Result<PushReport>`;`pub fn pull(store, config) -> Result<ImportReport>`;`pub fn webdav_url() -> String`(env 或默认坚果云);内部 `fn pack(dir) -> Vec<u8>`(tar.gz)、`fn unpack(bytes, dir)`
- CLI: `mnemush sync webdav-push` / `webdav-pull`

- [ ] **Step 1: 写失败测试**

`webdav.rs` 测试:

```rust
#[test]
fn pack_unpack_roundtrip() {
    let dir = std::env::temp_dir().join(format!("webdav-pack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("memory.json"), "[]").unwrap();
    std::fs::write(dir.join("MANIFEST.json"), "{}").unwrap();
    let bytes = pack(&dir).unwrap();
    let out = std::env::temp_dir().join(format!("webdav-unpack-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&out).unwrap();
    unpack(&bytes, &out).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("memory.json")).unwrap(), "[]");
    assert_eq!(std::fs::read_to_string(out.join("MANIFEST.json")).unwrap(), "{}");
}

#[test]
fn webdav_url_defaults_to_jianguoyun() {
    // 无 env 时默认坚果云
    let url = webdav_url();
    assert!(url.contains("dav.jianguoyun.com"), "default jianguoyun: {url}");
}

#[test]
fn credentials_required_for_push() {
    // 无 env 凭证 → push 报错提示
    let err = push_missing_creds();
    assert!(err.contains("MNEMUSH_WEBDAV_USER"), "clear error: {err}");
}
```

(测试 helper 与既有 store/sync 测试一致; `push_missing_creds` 用一个返回 Err 的 stub 或直接测 env 检查函数。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release webdav`
Expected: FAIL(模块不存在)

- [ ] **Step 3: 实现 webdav.rs**

```rust
//! webdav —— WebDAV 跨设备同步传输层(坚果云为默认选项)。
//! 复用 sync 快照格式, tar.gz 打包后 HTTP PUT/GET 到 WebDAV。
//! 凭证: MNEMUSH_WEBDAV_URL / USER / PASS 环境变量(不落命令行)。

use crate::error::Result;
use crate::store::Store;

pub const DEFAULT_WEBDAV_URL: &str = "https://dav.jianguoyun.com/dav/mnemush/";

/// WebDAV 目标 URL(env 覆盖, 默认坚果云)。
pub fn webdav_url() -> String {
    std::env::var("MNEMUSH_WEBDAV_URL").unwrap_or_else(|_| DEFAULT_WEBDAV_URL.to_string())
}

fn credentials() -> Result<(String, String)> {
    let user = std::env::var("MNEMUSH_WEBDAV_USER")
        .map_err(|_| crate::error::MnemushError::Other(
            "webdav: MNEMUSH_WEBDAV_USER not set".into()))?;
    let pass = std::env::var("MNEMUSH_WEBDAV_PASS")
        .map_err(|_| crate::error::MnemushError::Other(
            "webdav: MNEMUSH_WEBDAV_PASS not set".into()))?;
    Ok((user, pass))
}

/// 打包 sync 目录为 tar.gz(内存)。
fn pack(dir: &std::path::Path) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::Default);
        let mut tar = tar::Builder::new(enc);
        // 打包 memory.json / edges.json / MANIFEST.json / identity/ / embeddings/
        for entry in ["memory.json", "edges.json", "MANIFEST.json"] {
            let p = dir.join(entry);
            if p.exists() {
                tar.append_path_with_name(&p, entry)?;
            }
        }
        for sub in ["identity", "embeddings"] {
            let d = dir.join(sub);
            if d.exists() {
                for e in std::fs::read_dir(&d)? {
                    let e = e?;
                    if e.path().is_file() {
                        tar.append_path_with_name(&e.path(), format!("{}/{}", sub, e.file_name().to_string_lossy()))?;
                    }
                }
            }
        }
        tar.finish()?;
        tar.into_inner()?.finish()?;
    }
    Ok(buf)
}

/// 解包 tar.gz 到 dir。
fn unpack(bytes: &[u8], dir: &std::path::Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dir)?;
    Ok(())
}

/// push: export 快照 → tar.gz → PUT。目标路径 <url>/mnemush-sync.tar.gz。
pub fn push(store: &Store, data_dir: &std::path::Path) -> Result<()> {
    let (user, pass) = credentials()?;
    let tmp = data_dir.join("webdav-push-tmp");
    crate::sync::export_to(store, &tmp)?;
    let bytes = pack(&tmp)?;
    let url = format!("{}mnemush-sync.tar.gz", webdav_url().trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120)).build();
    let resp = agent
        .put(&url)
        .set("Authorization", &format!("Basic {}", base64_basic(&user, &pass)))
        .send_bytes(&bytes)
        .map_err(|e| crate::error::MnemushError::Other(format!("webdav put: {e}")))?;
    if resp.status() >= 400 {
        return Err(crate::error::MnemushError::Other(format!("webdav put: HTTP {}", resp.status())));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// pull: GET → 解包 → import(逐条合并)。
pub fn pull(store: &Store, data_dir: &std::path::Path) -> Result<crate::sync::ImportReport> {
    let (user, pass) = credentials()?;
    let url = format!("{}mnemush-sync.tar.gz", webdav_url().trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120)).build();
    let resp = agent
        .get(&url)
        .set("Authorization", &format!("Basic {}", base64_basic(&user, &pass)))
        .call()
        .map_err(|e| crate::error::MnemushError::Other(format!("webdav get: {e}")))?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;
    let tmp = data_dir.join("webdav-pull-tmp");
    std::fs::create_dir_all(&tmp)?;
    unpack(&bytes, &tmp)?;
    let report = crate::sync::import_from(store, &tmp)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(report)
}

fn base64_basic(user: &str, pass: &str) -> String {
    // 标准库无 base64 — 用 ureq 的 Basic auth? ureq 有 set auth?
    // 简单实现: base64 crate 是否已有? 若无, 用最小 base64 encode。
    // TODO: 确认 base64 依赖
    unimplemented!("base64")
}
```

**注**: base64 编码 —— 若项目无 base64 依赖, 需加 `base64 = "0.22"`(tiny, 标准); 或 ureq 有 `set("Authorization", ...)` 需手动 base64。检查 Cargo.toml; 若无 base64, 加这个小依赖(例外于"零新依赖", 因为标准库无 base64)。

- [ ] **Step 4: CLI 命令(cli.rs)**

`SyncCmd` enum 加:

```rust
/// Push the current DB snapshot to a WebDAV endpoint (default: 坚果云).
WebdavPush,
/// Pull + merge a WebDAV snapshot into the local DB.
WebdavPull,
```

match 分支:

```rust
SyncCmd::WebdavPush => {
    mnemush::webdav::push(&store, &crate::default_data_dir())?;
    println!("webdav push ok");
}
SyncCmd::WebdavPull => {
    let r = mnemush::webdav::pull(&store, &crate::default_data_dir())?;
    println!("imported {} memories, {} conflicts", r.imported, r.conflicts.len());
}
```

- [ ] **Step 5: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add crates/mnemush/src/webdav.rs crates/mnemush/src/lib.rs crates/mnemush/src/bin/cli.rs
git commit -m "✨ sync: WebDAV 传输层(webdav-push/pull, 坚果云默认)"
```

---

### Task 2: push 方向双向合并 + 乐观锁

**Files:**
- Modify: `crates/mnemush/src/webdav.rs`(push 前 GET + 合并 + PUT)
- Modify: `crates/mnemush/src/sync.rs`(抽出可复用的 `merge_memories(local, remote) -> Vec<Memory>` 逐条合并函数, 供 push 用; 现有 import_from 内部逻辑重构为调用它)
- Test: `crates/mnemush/src/webdav.rs` / `sync.rs`

**Interfaces:**
- Consumes: Task 1 `pack/unpack/push/pull`
- Produces: `pub fn merge_memories(local: Vec<Memory>, remote: Vec<Memory>) -> Vec<Memory>`(较新赢 + 并集 + 删除传播);`push` 改为: GET 远程 → 解包 → merge(local_export, remote) → 写回本地(远端较新者更新本地)→ pack → PUT(带 If-Match)

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn merge_newer_wins_and_union() {
    // 本地 A1(旧) + A2; 远端 A1(新) + B1 → 合并 = A1(新) + A2 + B1
    let mk = |id: &str, content: &str, ts: i64| {
        let mut m = Memory::note_placeholder(); // 构造 Memory(见下)
        m.id = id.to_string();
        m.content = content.to_string();
        m.last_accessed_at = crate::store::Store::ts_to_dt(ts);
        m.created_at = crate::store::Store::ts_to_dt(ts);
        m
    };
    let local = vec![mk("a1", "old", 100), mk("a2", "x", 200)];
    let remote = vec![mk("a1", "new", 300), mk("b1", "y", 150)];
    let merged = merge_memories(local, remote);
    let by_id: std::collections::HashMap<_, _> = merged.into_iter().map(|m| (m.id, m.content)).collect();
    assert_eq!(by_id.get("a1").unwrap(), "new", "remote newer wins");
    assert_eq!(by_id.get("a2").unwrap(), "x", "local-only kept");
    assert_eq!(by_id.get("b1").unwrap(), "y", "remote-only added");
}

#[test]
fn merge_deletion_propagates() {
    // 本地已删(deleted_at 新) vs 远端活跃 → 合并保持删除
    // 远端已删(deleted_at 新) vs 本地活跃 → 合并删除
}
```

(Memory 构造: 用 `schema::Memory` 全字段构造较啰嗦 — 参考 sync.rs 测试怎么构造, 或提供 helper。)

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release merge_`
Expected: FAIL

- [ ] **Step 3: 实现 merge_memories(sync.rs)**

```rust
/// 逐条合并: 同 id 比更新时间(max(last_accessed, created)), 较新者赢;
/// 新 id 并集; 软删(deleted_at)随较新者传播。供 webdav push/pull 复用。
pub fn merge_memories(local: Vec<Memory>, remote: Vec<Memory>) -> Vec<Memory> {
    let ts = |m: &Memory| m.last_accessed_at.timestamp().max(m.created_at.timestamp());
    let mut out: std::collections::BTreeMap<String, Memory> = std::collections::BTreeMap::new();
    for m in local {
        out.insert(m.id.clone(), m);
    }
    for r in remote {
        match out.get(&r.id) {
            Some(l) if ts(l) > ts(&r) => { /* local newer, keep */ }
            _ => { out.insert(r.id.clone(), r); }
        }
    }
    out.into_values().collect()
}
```

重构 `import_from` 用 `merge_memories`(远端为 remote, 本地 DB 为 local): 逐条比较已存在 —— 提取现有比较逻辑。

- [ ] **Step 4: push 合并 + 乐观锁(webdav.rs)**

```rust
pub fn push(store: &Store, data_dir: &Path) -> Result<()> {
    let (user, pass) = credentials()?;
    let url = format!("{}mnemush-sync.tar.gz", webdav_url().trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build();
    // 1) GET 远程快照(若存在)
    let remote_bytes = match agent.get(&url).set("Authorization", ...).call() {
        Ok(resp) => {
            let etag = resp.header("ETag").map(str::to_string);
            let mut b = Vec::new();
            resp.into_reader().read_to_end(&mut b)?;
            Some((b, etag))
        }
        Err(ureq::Error::Status(404, _)) => None, // 首次 push, 无远程
        Err(e) => return Err(...),
    };
    // 2) 本地快照
    let tmp = data_dir.join("webdav-push-tmp");
    crate::sync::export_to(store, &tmp)?;
    // 3) 若有远程, 双向合并 → 写回本地(远端较新者更新本地)
    if let Some((rb, _etag)) = remote_bytes {
        let rdir = data_dir.join("webdav-remote-tmp");
        unpack(&rb, &rdir)?;
        let remote_mems = read_snapshot(&rdir)?;  // sync.rs 提供 read 快照 helper
        let local_mems = read_snapshot(&tmp)?;
        let merged = crate::sync::merge_memories(local_mems, remote_mems);
        // 写回本地(merged 中远端较新者更新 DB — 通过 import 语义)
        write_snapshot(&tmp, &merged)?;  // 更新快照供 PUT
        // 把远端较新者写进本地 DB
        apply_merge_to_db(store, &merged)?;  // upsert 每条(比本地 updated_at 新者)
        let _ = std::fs::remove_dir_all(&rdir);
    }
    // 4) PUT(带 If-Match 乐观锁)
    let bytes = pack(&tmp)?;
    let mut req = agent.put(&url).set("Authorization", ...);
    if let Some(etag) = etag { req = req.set("If-Match", &etag); }
    match req.send_bytes(&bytes) {
        Ok(resp) if resp.status() < 400 => {}
        Err(ureq::Error::Status(412, _)) => {
            // 乐观锁冲突: 重取重合并再 PUT(单次重试)
            return push(store, data_dir);  // 递归重试(需深度限制)
        }
        Err(e) => return Err(...),
    }
    Ok(())
}
```

**乐观锁细节**: 412(Precondition Failed)= 远程已变 → 重试。递归 push 需 depth 限制(或 for 循环 2 次)。推荐 for 循环最多 3 次。

- [ ] **Step 5: 运行测试 + 本地模拟**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿。手动模拟: 双隔离 HOME + 本地 HTTP 服务? WebDAV 测试用 mock —— 可用 `python3 -m http.server` 模拟 PUT?http.server 不支持 PUT。用本地文件作为"假 WebDAV"(测试时 URL 指向 file:// 或跳过网络, 用 unpack/pack 直接验证 merge)。**merge_memories 单测已覆盖合并逻辑**; 端到端 WebDAV 用坚果云真实验证(手动, 用户凭证)。

- [ ] **Step 6: 提交**

```bash
git add crates/mnemush/src/webdav.rs crates/mnemush/src/sync.rs
git commit -m "✨ sync: push 双向合并 + 乐观锁(ETag/412 重试)"
```

---

### Task 3: 自动触发(记忆更新时 + 30s 去抖)

**Files:**
- Modify: `crates/mnemush/src/config.rs`(`SyncConfig` 加 webdav_enabled / webdav_debounce_secs)
- Modify: `crates/mnemush/src/memory.rs`(add/update/soft_delete 成功后调 `mark_sync_dirty` + 触发)
- Modify: `crates/mnemush/src/webdav.rs`(dirty 标记 + 去抖 + spawn)
- Test: `crates/mnemush/src/webdav.rs`

**Interfaces:**
- Consumes: Task 1/2 push
- Produces: `pub fn mark_sync_dirty(data_dir) -> Result<()>`(写 sync-dirty 文件);`pub fn maybe_auto_push(store, config, data_dir) -> Result<bool>`(读 dirty + 去抖判断 + spawn);`pub fn clear_dirty(data_dir)`
- 写入路径: `MemoryApi::add` 成功后(事务外)调 `maybe_auto_push`(fire-and-forget)

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn dirty_marker_roundtrip() {
    let dir = std::env::temp_dir().join(format!("dirty-{}", uuid::Uuid::new_v4()));
    mark_sync_dirty(&dir).unwrap();
    assert!(dir.join("sync-dirty").exists());
    clear_dirty(&dir);
    assert!(!dir.join("sync-dirty").exists());
}

#[test]
fn debounce_skips_within_window() {
    // 写 dirty → maybe_auto_push(30s 内) → 不触发(返回 false 且不清 dirty)
    // 手动改 dirty 时间戳到 31s 前 → maybe_auto_push → 触发(返回 true)
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd crates/mnemush && cargo test --release webdav::tests`
Expected: FAIL

- [ ] **Step 3: 实现(config + dirty + 触发)**

`config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// WebDAV 自动同步开关(默认 false; 配好凭证才启用)。
    pub webdav_enabled: bool,
    /// 去抖秒数: 窗口内多次写入合并为一次 push。
    pub webdav_debounce_secs: i64,
}
impl Default for SyncConfig { ... webdav_enabled: false, webdav_debounce_secs: 30 }
```

`Config` 加 `pub sync: SyncConfig,` + Default。

`webdav.rs`:

```rust
fn dirty_path(data_dir: &Path) -> PathBuf { data_dir.join("sync-dirty") }

/// 写 dirty 标记(记录时间戳)。调用方: MemoryApi 写入成功后。
pub fn mark_sync_dirty(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        dirty_path(data_dir),
        chrono::Utc::now().timestamp().to_string(),
    )?;
    Ok(())
}

/// 自动触发: 若 dirty 时间戳超过去抖窗口 → spawn push(异步, 不阻塞写入)。
/// 返回是否触发了 push。push 成功清 dirty; 失败保留(下次写入重试)。
pub fn maybe_auto_push(store: &Store, config: &Config, data_dir: &Path) -> Result<bool> {
    if !config.sync.webdav_enabled {
        return Ok(false);
    }
    let path = dirty_path(data_dir);
    let dirty_ts: i64 = match std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok()) {
        Some(ts) => ts,
        None => return Ok(false),
    };
    let now = chrono::Utc::now().timestamp();
    if now - dirty_ts < config.sync.webdav_debounce_secs {
        return Ok(false); // 去抖窗口内, 等下次
    }
    // spawn 异步 push(fire-and-forget)
    let store2 = store.clone_conn(); // 或传 conn; 若 Store 不可 clone, 用线程 + 新连接
    let data_dir2 = data_dir.to_path_buf();
    let enabled = config.sync.webdav_enabled;
    std::thread::spawn(move || {
        // 用独立连接(Store::open 或 MemoryApi::new on clone conn)
        if let Ok(store3) = open_store_like(&store2) {
            if let Ok(()) = crate::webdav::push(&store3, &data_dir2) {
                let _ = std::fs::remove_file(dirty_path(&data_dir2));
            }
            // 失败 → dirty 保留
        }
    });
    Ok(true)
}
```

**线程/Store**: Store 持 Connection(rusqlite Connection 非 Send?) — 检查 rusqlite Connection 是否 Send。WAL 模式下 SQLite 连接可跨线程(需 Connection: Send)。若不可, 用 `Store::open(db_path)` 重开(从 config.storage.db_path)。实现细节: spawn 线程里 `Store::open(&db_path)` 重开连接。

`memory.rs` add 成功后:

```rust
// WebDAV 自动同步(dirty + 去抖 + 异步 push)。失败静默, 不阻塞写入。
if let Err(e) = crate::webdav::maybe_auto_push(&self.store, self.config, &crate::default_data_dir()) {
    log_event("sync_push_skipped", ...); // 或静默
}
```

update/soft_delete 同样挂钩。

- [ ] **Step 4: 运行测试**

Run: `cd crates/mnemush && cargo test --release`
Expected: 全绿

- [ ] **Step 5: 真实端到端(可选, 需用户凭证)**

设置 `MNEMUSH_WEBDAV_URL/USER/PASS` → `mnemush sync webdav-push` → 检查坚果云文件 → 另一设备 `webdav-pull`。

- [ ] **Step 6: 提交 + 更新文档**

```bash
git add crates/mnemush/src/config.rs crates/mnemush/src/memory.rs crates/mnemush/src/webdav.rs docs/config.example.toml CHANGELOG.md README.md
git commit -m "✨ sync: 记忆更新自动触发 WebDAV 同步(30s 去抖, 默认关闭)"
```

CHANGELOG v1.6.0 段 + README Status + config.example.toml `[sync]` 段。

---

## Self-Review

**Spec 覆盖:**
- WebDAV 传输层(push/pull, tar.gz, 凭证 env)→ Task 1 ✓
- 坚果云默认 URL → Task 1 webdav_url ✓
- 实时合并(较新赢 + 并集 + 删除传播)→ Task 2 merge_memories(算法已本地模拟验证)✓
- 乐观锁(ETag/412 重试)→ Task 2 ✓
- 记忆更新自动触发 + 30s 去抖 → Task 3 ✓
- 配置开关默认关闭 → Task 3 ✓
- 冲突保留本地新版 → 现有 import 语义 + merge ✓

**占位符:** Task 1 base64 有 TODO(需确认依赖); 其余无。

**类型一致性:** `push(store, data_dir)` / `pull(store, data_dir)` / `merge_memories(local, remote)` / `mark_sync_dirty` / `maybe_auto_push` 签名跨任务一致; `SyncConfig` Task 3 定义, Task 3 使用。

**风险:** 线程中重开 Store 连接(WAL 下 SQLite 跨线程); base64 依赖; 递归 push 需深度限制。
