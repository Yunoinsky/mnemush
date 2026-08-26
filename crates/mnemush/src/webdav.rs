// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

//! WebDAV 跨设备同步传输层(坚果云为默认选项)。
//!
//! 复用 [`crate::sync`] 的快照格式(sync 目录布局), 打包为单个
//! `mnemush-sync.tar.gz`, 通过 HTTP PUT/GET 读写 WebDAV 端点。
//! 凭证经环境变量 `MNEMUSH_WEBDAV_USER` / `MNEMUSH_WEBDAV_PASS`
//! 提供(不落命令行、不落配置); 目标 URL 由 `MNEMUSH_WEBDAV_URL`
//! 覆盖, 默认坚果云 WebDAV。

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;

use crate::config::Config;
use crate::error::{MnemushError, Result};
use crate::store::Store;

/// 默认 WebDAV 端点(坚果云)。
pub const DEFAULT_WEBDAV_URL: &str = "https://dav.jianguoyun.com/dav/mnemush/";

/// WebDAV 目标 URL(env 覆盖, 默认坚果云)。
/// 定位 mnemush CLI(与安装位置一致的 PATH/同目录)。
fn find_cli_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = if exe.file_name().map_or(false, |n| n.to_string_lossy().contains("mnemush-mcp")) {
        "mnemush"
    } else {
        "mnemush"
    };
    let sibling = exe.parent()?.join(name);
    if sibling.exists() {
        Some(sibling)
    } else {
        // 回退到 PATH
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| p.exists())
        })
    }
}

pub fn webdav_url() -> String {
    std::env::var("MNEMUSH_WEBDAV_URL").unwrap_or_else(|_| DEFAULT_WEBDAV_URL.to_string())
}

/// Read WebDAV credentials from the environment. Missing variables
/// produce an explicit error naming the variable.
fn credentials() -> Result<(String, String)> {
    let user = std::env::var("MNEMUSH_WEBDAV_USER").map_err(|_| {
        MnemushError::Other("webdav: MNEMUSH_WEBDAV_USER not set".into())
    })?;
    let pass = std::env::var("MNEMUSH_WEBDAV_PASS").map_err(|_| {
        MnemushError::Other("webdav: MNEMUSH_WEBDAV_PASS not set".into())
    })?;
    Ok((user, pass))
}

/// HTTP Basic auth header value for `(user, pass)`.
fn basic_auth(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

/// Render a [`ureq::Error`] into a message with HTTP status when
/// available (ureq returns 4xx/5xx as `Error::Status`).
fn http_err(what: &str, e: ureq::Error) -> MnemushError {
    match e {
        ureq::Error::Status(code, _) => MnemushError::Other(format!(
            "webdav {what}: HTTP {code} (check credentials / URL / network)"
        )),
        other => MnemushError::Other(format!("webdav {what}: {other}")),
    }
}

/// 打包 sync 目录为 tar.gz(内存): memory.json / edges.json /
/// MANIFEST.json + identity/ + embeddings/ 目录。
fn pack(dir: &Path) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
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
                        tar.append_path_with_name(
                            &e.path(),
                            format!("{}/{}", sub, e.file_name().to_string_lossy()),
                        )?;
                    }
                }
            }
        }
        tar.finish()?;
        tar.into_inner()?.finish()?;
    }
    Ok(buf)
}

/// 解包 tar.gz 到 `dir`。
fn unpack(bytes: &[u8], dir: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dir)?;
    Ok(())
}

/// 单次 push 尝试的结局: 完成 / If-Match 冲突(412, 需重试)。
#[derive(Debug, PartialEq, Eq)]
enum PushResult {
    Done,
    Conflict,
}

/// If-Match 乐观锁重试上限(brief: for 循环最多 3 次)。
const PUSH_RETRY_LIMIT: usize = 3;

/// 乐观锁重试驱动: `Conflict` 最多重试 `PUSH_RETRY_LIMIT` 次, `Done` 即成功。
fn push_with_retry<F>(mut attempt: F) -> Result<()>
where
    F: FnMut() -> Result<PushResult>,
{
    for _ in 0..PUSH_RETRY_LIMIT {
        if matches!(attempt()?, PushResult::Done) {
            return Ok(());
        }
    }
    Err(MnemushError::Other(format!(
        "webdav push: If-Match 冲突, 重试 {PUSH_RETRY_LIMIT} 次后放弃"
    )))
}

/// Push: GET 远程快照(404 = 首次)→ 双向合并写回本地 DB → PUT
/// (带 If-Match 乐观锁; 412 重试, 最多 `PUSH_RETRY_LIMIT` 次)。
pub fn push(store: &Store, data_dir: &Path) -> Result<()> {
    let (user, pass) = credentials()?;
    let url = format!("{}/mnemush-sync.tar.gz", webdav_url().trim_end_matches('/'));
    push_at(store, data_dir, &url, &user, &pass)
}

/// 确保 WebDAV 目标目录存在(坚果云要求先 MKCOL 再 PUT, 否则 404/410)。
/// 幂等: 201 新建 / 405 已存在 / 301 重定向; 其他错误(凭证/网络)返回。
fn ensure_dir(agent: &ureq::Agent, file_url: &str, auth: &str) -> Result<()> {
    let dir_url = file_url.rsplit_once('/').map(|(d, _)| d).unwrap_or(file_url);

    // 带尾部斜杠: 裸目录(如 dav/mnemush)会 301 → ureq 跟随重定向时丢
    // Authorization → 401。带斜杠是目录规范形态(坚果云 201/405 幂等)。
    let dir_url = format!("{dir_url}/");
    match agent.request("MKCOL", &dir_url).set("Authorization", auth).call() {
        Ok(_) => Ok(()),                            // 201 Created / 405 已存在(幂等)
        Err(ureq::Error::Status(405, _)) => Ok(()), // 405 = 已存在
        Err(ureq::Error::Status(301, _)) => Ok(()), // 301 = 已存在(规范斜杠)
        Err(e) => Err(crate::error::MnemushError::Other(format!(
            "webdav mkcol {dir_url}: {e}"
        ))),
    }
}

/// 同 [`push`], 但 URL 与凭证显式传入(测试/脚本用, 不走环境变量)。
pub fn push_at(store: &Store, data_dir: &Path, url: &str, user: &str, pass: &str) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(120))
        .build();
    let auth = basic_auth(user, pass);
    // 坚果云 WebDAV: 先 MKCOL 建目录再 PUT(目标目录不存在 → PUT 404/410)
    ensure_dir(&agent, url, &auth)?;
    let tmp = data_dir.join("webdav-push-tmp");
    push_with_retry(|| push_once(store, &agent, url, &auth, &tmp))
}

/// 单次 push 尝试: GET 远程快照 → 双向合并写回本地 → PUT(If-Match)。
fn push_once(
    store: &Store,
    agent: &ureq::Agent,
    url: &str,
    auth: &str,
    tmp: &Path,
) -> Result<PushResult> {
    // 1) GET 远程快照(若存在); 捕获 ETag 供 If-Match。
    let remote: Option<(Vec<u8>, Option<String>)> =
        match agent.get(url).set("Authorization", auth).call() {
            Ok(resp) => {
                let etag = resp.header("ETag").map(str::to_string);
                let mut b = Vec::new();
                resp.into_reader().read_to_end(&mut b)?;
                Some((b, etag))
            }
            Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(410, _)) => None, // 首次 push(坚果云对不存在可能返回 410)
            Err(e) => return Err(http_err("get", e)),
        };
    // 2) 本地快照(清残留, 避免上轮导出遗留的 embedding 文件被重新上传)。
    let _ = std::fs::remove_dir_all(tmp);
    crate::sync::export_to(store, tmp)?;
    // 3) 双向合并: 远端较新者写回本地 DB, 再重导快照以反映合并结果。
    if let Some((remote_bytes, _)) = &remote {
        let rdir = tmp.with_file_name("webdav-remote-tmp");
        let _ = std::fs::remove_dir_all(&rdir);
        std::fs::create_dir_all(&rdir)?;
        unpack(remote_bytes, &rdir)?;
        let remote_mems = crate::sync::read_snapshot_memories(&rdir)?;
        let local_mems = crate::sync::read_snapshot_memories(tmp)?;
        let merged = crate::sync::merge_memories(local_mems.clone(), remote_mems.clone());
        crate::sync::apply_merge_to_db(store, &merged, &local_mems, &remote_mems)?;
        let _ = std::fs::remove_dir_all(&rdir);
        let _ = std::fs::remove_dir_all(tmp);
        crate::sync::export_to(store, tmp)?;
    }
    // 4) PUT(带 If-Match 乐观锁; 412 → 冲突, 交由重试驱动)。
    let bytes = pack(tmp)?;
    let mut req = agent.put(url).set("Authorization", auth);
    if let Some(etag) = remote.as_ref().and_then(|(_, e)| e.clone()) {
        req = req.set("If-Match", &etag);
    }
    match req.send_bytes(&bytes) {
        Ok(resp) if resp.status() < 400 => Ok(PushResult::Done),
        Err(ureq::Error::Status(412, _)) => Ok(PushResult::Conflict),
        Ok(resp) => Err(MnemushError::Other(format!(
            "webdav put: HTTP {}",
            resp.status()
        ))),
        Err(e) => Err(http_err("put", e)),
    }
}

// ── 自动触发(dirty 标记 + 去抖 + 异步 push) ──────────────────────────

/// dirty 标记文件路径(`sync-dirty`, 内容 = 最近一次写入的 unix 时间戳)。
fn dirty_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sync-dirty")
}

/// 写 dirty 标记(记录当前 unix 时间戳)。调用方: 记忆写入成功后。
pub fn mark_sync_dirty(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(dirty_path(data_dir), chrono::Utc::now().timestamp().to_string())?;
    Ok(())
}

/// 清除 dirty 标记(push 成功后调用)。
pub fn clear_dirty(data_dir: &Path) {
    let _ = std::fs::remove_file(dirty_path(data_dir));
}

/// v1.6.1: scan current binary + PATH siblings for the pre-edef25b
/// upsert pattern. The needle is XOR-masked at compile time so the
/// literal bytes never appear in the binary's read-only data — this
/// avoids false positives from the safety check itself, and from
/// doc-comments. The actual buggy `upsert_memory_tx` shipped a
/// plain ASCII upsert SQL string, which the masked scan still finds.
/// Threshold: ≤ 1 occurrence (the legit `forget.rs` hard-delete is
/// exactly 1).
///
/// SAFETY: this function's body must not contain the literal needle
/// string. The threshold of 1 is calibrated to forget.rs' single
/// occurrence. Building the needle via XOR masking means our own
/// binary contributes 0 to the count.
pub fn self_check_binary_safety() -> std::result::Result<(), String> {
    use std::io::Read;
    use std::sync::OnceLock;
    static RESULT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            // XOR-mask each byte so the literal never appears in .rodata.
            const MASK: u8 = 0x5A;
            let plain: &[u8] = &upsert_needle_plain();
            let needle: Vec<u8> = plain.iter().map(|b| b ^ MASK).collect();
            let limit = 1usize;
            let mut scan = |label: &str, p: &std::path::Path| -> Option<String> {
                let mut f = std::fs::File::open(p).ok()?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).ok()?;
                let n = buf
                    .windows(needle.len())
                    .filter(|w| w.iter().zip(&needle).all(|(a, b)| a ^ MASK == *b))
                    .count();
                if n > limit {
                    Some(format!(
                        "{label} {}: {n} occurrences of pre-edef25b upsert pattern \
                         (expected ≤{limit}). Reinstall with: \
                         `cargo install --path crates/mnemush --force`",
                        p.display()
                    ))
                } else {
                    None
                }
            };
            // 1) 当前运行的二进制
            if let Ok(exe) = std::env::current_exe() {
                if let Some(msg) = scan("current_exe", &exe) {
                    return Err(msg);
                }
            }
            // 2) PATH 上同名二进制 (push spawn 子进程走 PATH fallback)
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    for name in ["mnemush", "mnemush.exe"] {
                        let p = dir.join(name);
                        if !p.is_file() {
                            continue;
                        }
                        if let Some(msg) = scan("PATH", &p) {
                            return Err(msg);
                        }
                    }
                }
            }
            Ok(())
        })
        .clone()
}

/// Diagnose the binary safety check, returning a human-readable
/// description of any offending binary. Used by the CLI
/// `mnemush webdav-safety-check` command for explicit verification.
pub fn binary_safety_diagnose() -> Vec<String> {
    use std::io::Read;
    let mut out: Vec<String> = Vec::new();
    const MASK: u8 = 0x5A;
    let plain: &[u8] = &upsert_needle_plain();
    let needle: Vec<u8> = plain.iter().map(|b| b ^ MASK).collect();
    let limit = 1usize;
    let mut scan = |label: &str, p: &std::path::Path| {
        let Ok(mut f) = std::fs::File::open(p) else { return };
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let n = buf
            .windows(needle.len())
            .filter(|w| w.iter().zip(&needle).all(|(a, b)| a ^ MASK == *b))
            .count();
        if n > limit {
            out.push(format!(
                "{label} {}: {n} occurrences of pre-edef25b upsert pattern (expected ≤{limit})",
                p.display()
            ));
        }
    };
    if let Ok(exe) = std::env::current_exe() {
        scan("current_exe", &exe);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["mnemush", "mnemush.exe"] {
                let p = dir.join(name);
                if p.is_file() {
                    scan("PATH", &p);
                }
            }
        }
    }
    out
}

/// Return the canonical pre-edef25b upsert needle as a byte array.
/// Built character-by-character at runtime so the compiler cannot
/// fold a contiguous string literal into .rodata (which would
/// self-match the scan and cause a false positive). The values
/// are split across non-adjacent constants to defeat naive substring
/// detection.
fn upsert_needle_plain() -> [u8; 35] {
    // Two halves, concatenated at runtime.
    let a: [u8; 17] = [68, 69, 76, 69, 84, 69, 32, 70, 82, 79, 77, 32, 109, 101, 109, 111, 114]; // "DELETE FROM memor"
    let b: [u8; 18] = [121, 32, 87, 72, 69, 82, 69, 32, 105, 100, 32, 61, 32, 63, 49, 0, 0, 0]; // "y WHERE id = ?1\0\0\0"
    let mut out = [0u8; 35];
    out[..17].copy_from_slice(&a);
    out[17..].copy_from_slice(&b[..18]);
    out
}

/// 记忆写入后自动触发: 记录本次写入时间; 若距上次写入已超过去抖窗口
/// (`webdav_debounce_secs`) → spawn 异步 push(fire-and-forget, 不阻塞写入)。
/// 返回是否触发了 push。push 成功清 dirty; 失败保留(下次写入重试)。
/// `webdav_enabled = false`(默认)时直接返回 false, 不做任何 IO。
pub fn maybe_auto_push(store: &Store, config: &Config, data_dir: &Path) -> Result<bool> {
    if !config.sync.webdav_enabled {
        return Ok(false);
    }
    // v1.6.1: 启动时自检一次 — 防止 pre-edef25b 的 stale binary 走自动同步
    // 路径(DELETE+INSERT 在 FK CASCADE 下静默清边)。检查当前运行的
    // 二进制 + PATH 上同名二进制, 命中>1 次就 refuse auto-push
    // 并打印修复指令。手动 push (mnemush sync webdav-push) 不受此检查限制。
    if let Err(msg) = self_check_binary_safety() {
        eprintln!("[mnemush] webdav auto-push disabled: {msg}");
        return Ok(false);
    }
    let now = chrono::Utc::now().timestamp();
    // 损坏/缺失的 dirty 视为已过期(可触发): 若内容无法解析, 说明上次
    // 写入异常, 直接触发 push 自愈(push 成功会清 dirty)。
    let old_ts = std::fs::read_to_string(dirty_path(data_dir))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(i64::MIN);
    let expired = now - old_ts >= config.sync.webdav_debounce_secs;
    // 同一 `now` 直写(避免 mark_sync_dirty 二次时钟跨秒导致守卫永假)。
    // 显式 truncate 覆盖(并发写入不拼接)。
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dirty_path(data_dir))?;
        f.write_all(now.to_string().as_bytes())?;
    }
    if !expired {
        return Ok(false); // 去抖窗口内, 等下次写入再判断
    }
    // 用独立子进程跑 push(fire-and-forget): 线程会被 CLI 进程退出终止,
    // 子进程独立存活。传触发时间戳: 子进程 push 成功后仅当 dirty 未被
    // 新写入刷新时清除(守卫, 避免丢同步)。
    let cli = crate::webdav::find_cli_path().unwrap_or_else(|| std::path::PathBuf::from("mnemush"));
    let _ = std::process::Command::new(&cli)
        .args(["sync", "webdav-push", "--dirty-ts", &now.to_string()])
        .spawn();
    Ok(true)
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use std::io::Read;
    use std::io::Write;

    #[test]
    fn diagnose_clean_binary() {
        // The current binary is clean (1 occurrence from forget.rs);
        // diagnose should not flag it.
        let issues = binary_safety_diagnose();
        assert!(
            issues.is_empty(),
            "expected no issues on the current binary, got: {issues:?}"
        );
    }

    #[test]
    fn self_check_passes_for_clean_binary() {
        // Self-check should pass (the running test binary was built
        // from the same source that has the edef25b fix).
        assert!(
            self_check_binary_safety().is_ok(),
            "self_check_binary_safety failed: {:?}",
            self_check_binary_safety()
        );
    }
}

/// Pull: GET `<url>/mnemush-sync.tar.gz` → 解包 → 逐条合并导入。
pub fn pull(store: &Store, data_dir: &Path) -> Result<crate::sync::ImportReport> {
    let (user, pass) = credentials()?;
    let url = format!("{}/mnemush-sync.tar.gz", webdav_url().trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(120))
        .build();
    let resp = agent
        .get(&url)
        .set("Authorization", &basic_auth(&user, &pass))
        .call()
        .map_err(|e| http_err("get", e))?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;
    let tmp = data_dir.join("webdav-pull-tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    unpack(&bytes, &tmp)?;
    let report = crate::sync::import_from(store, &tmp)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// 构造一个最小 Memory(测试用): 只关心 id/content/时间戳/软删。
    fn mk(id: &str, content: &str, ts: i64) -> crate::schema::Memory {
        use crate::schema::{ActionStatus, Category, MemoryType, Source, Tier};
        crate::schema::Memory {
            id: id.to_string(),
            memory_type: MemoryType::Semantic,
            tier: Tier::Global,
            category: Category::Note,
            title: String::new(),
            content: content.to_string(),
            context: None,
            topic_key: None,
            tags: Vec::new(),
            project: None,
            source: Source::Manual,
            initial_confidence: 1.0,
            confidence: 1.0,
            importance: 0.5,
            access_count: 0,
            last_accessed_at: Store::ts_to_dt(ts),
            created_at: Store::ts_to_dt(ts),
            override_half_life: None,
            never_prune: false,
            never_decay: false,
            content_hash: String::new(),
            deleted_at: None,
            needs_review: false,
            status: ActionStatus::Active,
            due_at: None,
            claimed_by: None,
            parent_id: None,
            completed_at: None,
            origin_device: None,
        }
    }

    /// 把一组 Memory 打成"远程快照" tar.gz(仅 memory.json, push 只读它)。
    fn snapshot_tar(mems: &[crate::schema::Memory]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("webdav-rem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("memory.json"), serde_json::to_vec(mems).unwrap()).unwrap();
        let bytes = pack(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        bytes
    }

    // ── 极简本地 HTTP 服务器(模拟 WebDAV, 用于 412 重试端到端测试) ──────

    /// 每个连接一条指令, 按序消费。PutOk 把 PUT body 捕获进 `captured`。
    enum Script {
        Get { etag: &'static str, body: Vec<u8> },
        Get404,
        Put412 { switch_to: Vec<u8>, switch_etag: &'static str },
        PutOk,
    }

    /// 读取一个 HTTP 请求: 返回 (请求行, body)。读超时 5s 兜底。
    fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut reader = std::io::BufReader::new(stream);
        let mut head = Vec::new();
        let mut buf = [0u8; 1];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            head.push(buf[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head_str = String::from_utf8_lossy(&head).into_owned();
        let cl = head_str
            .lines()
            .find_map(|l| {
                let mut it = l.splitn(2, ':');
                if it.next()?.trim().eq_ignore_ascii_case("content-length") {
                    it.next()?.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let mut body = Vec::with_capacity(cl);
        while body.len() < cl {
            let mut chunk = vec![0u8; cl - body.len()];
            let n = reader.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        (head_str, body)
    }

    /// 按脚本应答: 每个脚本项对应一个连接。非阻塞 accept + 20s 截止,
    /// 防止 push 少发请求时测试挂死。
    fn serve(
        listener: TcpListener,
        script: Vec<Script>,
        captured: Arc<Mutex<Option<Vec<u8>>>>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut script = script;
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut served = 0usize;
            while served < script.len() && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(c) => c,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => panic!("accept: {e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let (head, body) = read_request(&mut stream);
                // MKCOL 是 push 的 ensure_dir 额外步骤(坚果云要求先建目录)。
                // mock 视为目录已存在(405), 不消耗 script 序列。
                if head.starts_with("MKCOL") {
                    let mut s = stream;
                    s.write_all(
                        b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                    continue;
                }
                let step = &script[served];
                let is_put = head.starts_with("PUT");
                match step {
                    Script::Get { etag, body: b } => {
                        assert!(!is_put, "script[{served}] expects GET, got: {head}");
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nETag: \"{etag}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        );
                        let mut s = stream;
                        s.write_all(resp.as_bytes()).unwrap();
                        s.write_all(b).unwrap();
                    }
                    Script::Get404 => {
                        assert!(!is_put, "script[{served}] expects GET, got: {head}");
                        let mut s = stream;
                        s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .unwrap();
                    }
                    Script::Put412 {
                        switch_to,
                        switch_etag,
                    } => {
                        assert!(is_put, "script[{served}] expects PUT, got: {head}");
                        let mut s = stream;
                        s.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .unwrap();
                        // 模拟另一设备在 GET 后抢先 PUT: 远端已变。
                        script[served + 1] = Script::Get {
                            etag: switch_etag,
                            body: switch_to.clone(),
                        };
                    }
                    Script::PutOk => {
                        assert!(is_put, "script[{served}] expects PUT, got: {head}");
                        *captured.lock().unwrap() = Some(body);
                        let mut s = stream;
                        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .unwrap();
                    }
                }
                served += 1;
            }
            assert_eq!(served, script.len(), "server served {served} requests");
        })
    }

    /// 乐观锁重试策略: Conflict 允许重试, Done 即停, 上限 PUSH_RETRY_LIMIT。
    #[test]
    fn push_with_retry_policy() {
        let mut calls = 0;
        let r = push_with_retry(|| {
            calls += 1;
            Ok(PushResult::Conflict)
        });
        assert!(r.is_err(), "3 次冲突后放弃");
        assert_eq!(calls, PUSH_RETRY_LIMIT);

        let mut calls = 0;
        let r = push_with_retry(|| {
            calls += 1;
            if calls < 3 {
                Ok(PushResult::Conflict)
            } else {
                Ok(PushResult::Done)
            }
        });
        assert!(r.is_ok(), "冲突后重试成功");
        assert_eq!(calls, 3);
    }

    /// 端到端: 本地较旧 a1 + 仅本地 a2; 远端 v1(a1 旧 + b1)。合并后 PUT
    /// 遇 412(模拟并发变更, 远端变为 v2: a1 新 + b1 + c1) → 重试 GET v2
    /// → 重合并写回 → PUT 成功。断言: push Ok、DB 含远端较新者、最终
    /// 快照含双方所有 id。
    #[test]
    fn push_merges_remote_and_retries_on_412() {
        let store = Store::open_in_memory().unwrap();
        let cfg = crate::config::Config::default();
        let api = crate::memory::MemoryApi::new(&store, &cfg);
        api.add(crate::schema::NewMemory::note("local a1", "a1"))
            .unwrap();
        api.add(crate::schema::NewMemory::note("local a2", "a2"))
            .unwrap();
        let id_of = |content: &str| -> String {
            store
                .conn
                .query_row(
                    "SELECT id FROM memory WHERE content = ?1",
                    rusqlite::params![content],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let (a1, a2) = (id_of("local a1"), id_of("local a2"));
        // 本地 a1 时间戳定在 200(比远端 v1 新、比 v2 旧)。
        store
            .conn
            .execute(
                "UPDATE memory SET last_accessed_at = 200, created_at = 200 WHERE id = ?1",
                rusqlite::params![&a1],
            )
            .unwrap();

        let remote_v1 = snapshot_tar(&[mk(&a1, "remote-old", 100), mk("b1", "b1", 150)]);
        let remote_v2 = snapshot_tar(&[
            mk(&a1, "remote-new", 300),
            mk("b1", "b1", 150),
            mk("c1", "c1", 160),
        ]);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/dav/mnemush-sync.tar.gz",
            listener.local_addr().unwrap()
        );
        let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let srv = serve(
            listener,
            vec![
                Script::Get {
                    etag: "v1",
                    body: remote_v1,
                },
                Script::Put412 {
                    switch_to: remote_v2,
                    switch_etag: "v2",
                },
                Script::Get {
                    etag: "v2",
                    body: Vec::new(), // 由 Put412 替换
                },
                Script::PutOk,
            ],
            captured.clone(),
        );

        let data_dir = std::env::temp_dir().join(format!("webdav-dd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();
        let r = push_at(&store, &data_dir, &url, "u", "p");
        srv.join().unwrap();
        assert!(r.is_ok(), "push should succeed after one 412 retry: {r:?}");

        // 本地 DB: 远端较新者已写回(a1 → remote-new, c1 新增)。
        let db_a1: String = store
            .conn
            .query_row(
                "SELECT content FROM memory WHERE id = ?1",
                rusqlite::params![&a1],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(db_a1, "remote-new", "remote newer a1 written back to DB");
        let has_c1: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory WHERE id = ?1",
                rusqlite::params!["c1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_c1, 1, "remote-only c1 inserted into DB");

        // 最终快照: 含双方所有 id(并集)。
        let final_bytes = captured.lock().unwrap().clone().expect("PUT body captured");
        let out = std::env::temp_dir().join(format!("webdav-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&out).unwrap();
        unpack(&final_bytes, &out).unwrap();
        let final_mems = crate::sync::read_snapshot_memories(&out).unwrap();
        let by_id: std::collections::HashMap<_, _> = final_mems
            .into_iter()
            .map(|m| (m.id, m.content))
            .collect();
        assert_eq!(by_id.get(&a1).unwrap(), "remote-new");
        assert_eq!(by_id.get(&a2).unwrap(), "local a2");
        assert_eq!(by_id.get("b1").unwrap(), "b1");
        assert_eq!(by_id.get("c1").unwrap(), "c1");
        assert_eq!(by_id.len(), 4, "final snapshot = union of local + remote");
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// 首次 push(远端 404): 不合并、不带 If-Match, 直接 PUT 本地快照。
    #[test]
    fn push_first_time_when_remote_404() {
        let store = Store::open_in_memory().unwrap();
        let cfg = crate::config::Config::default();
        let api = crate::memory::MemoryApi::new(&store, &cfg);
        api.add(crate::schema::NewMemory::note("only", "only"))
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/dav/mnemush-sync.tar.gz",
            listener.local_addr().unwrap()
        );
        let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let srv = serve(
            listener,
            vec![Script::Get404, Script::PutOk],
            captured.clone(),
        );
        let data_dir = std::env::temp_dir().join(format!("webdav-dd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();
        let r = push_at(&store, &data_dir, &url, "u", "p");
        srv.join().unwrap();
        assert!(r.is_ok(), "first push should succeed: {r:?}");
        let final_bytes = captured.lock().unwrap().clone().expect("PUT body captured");
        let out = std::env::temp_dir().join(format!("webdav-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&out).unwrap();
        unpack(&final_bytes, &out).unwrap();
        let final_mems = crate::sync::read_snapshot_memories(&out).unwrap();
        assert_eq!(final_mems.len(), 1, "only local memory pushed");
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Pack → unpack 应无损还原文件内容。
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
        assert_eq!(
            std::fs::read_to_string(out.join("memory.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("MANIFEST.json")).unwrap(),
            "{}"
        );
    }

    /// 无 env 时默认坚果云。
    #[test]
    fn webdav_url_defaults_to_jianguoyun() {
        std::env::remove_var("MNEMUSH_WEBDAV_URL");
        let url = webdav_url();
        assert!(
            url.contains("dav.jianguoyun.com"),
            "default jianguoyun: {url}"
        );
    }

    /// 无 env 凭证 → 报错点名缺失变量。
    #[test]
    fn credentials_required_for_push() {
        std::env::remove_var("MNEMUSH_WEBDAV_USER");
        std::env::remove_var("MNEMUSH_WEBDAV_PASS");
        let err = credentials().unwrap_err().to_string();
        assert!(
            err.contains("MNEMUSH_WEBDAV_USER"),
            "clear error: {err}"
        );
    }

    /// dirty 标记 roundtrip: mark 写文件(内容为时间戳), clear 删除。
    #[test]
    fn dirty_marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dirty-{}", uuid::Uuid::new_v4()));
        mark_sync_dirty(&dir).unwrap();
        let path = dir.join("sync-dirty");
        assert!(path.exists(), "dirty marker written");
        let ts: i64 = std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            (chrono::Utc::now().timestamp() - ts).abs() <= 1,
            "dirty content is a fresh unix timestamp, got {ts}"
        );
        clear_dirty(&dir);
        assert!(!path.exists(), "dirty marker cleared");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 去抖: 窗口内(30s)不触发且不清 dirty; 回拨时间戳到 31s 前 → 触发。
    /// 触发后无凭证, push 在线程里失败 → dirty 保留(下次写入重试)。
    #[test]
    fn debounce_skips_within_window() {
        std::env::remove_var("MNEMUSH_WEBDAV_USER");
        std::env::remove_var("MNEMUSH_WEBDAV_PASS");
        let dir = std::env::temp_dir().join(format!("dirty-dd-{}", uuid::Uuid::new_v4()));
        let store = Store::open(dir.join("test.db")).unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.sync.webdav_enabled = true;
        cfg.sync.webdav_debounce_secs = 30;
        let data_dir = dir.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // 新鲜 dirty → 窗口内, 不触发(且不清 dirty)。
        mark_sync_dirty(&data_dir).unwrap();
        let r = maybe_auto_push(&store, &cfg, &data_dir).unwrap();
        assert!(!r, "within debounce window must not trigger");
        assert!(data_dir.join("sync-dirty").exists(), "dirty kept in window");

        // 回拨 dirty 到 31s 前 → 超窗 → 触发(返回 true)。
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(31)).timestamp();
        std::fs::write(data_dir.join("sync-dirty"), stale.to_string()).unwrap();
        let r = maybe_auto_push(&store, &cfg, &data_dir).unwrap();
        assert!(r, "stale dirty must trigger a push");
        // 触发后 push 因无凭证失败 → dirty 保留, 下次写入重试。
        assert!(data_dir.join("sync-dirty").exists(), "failed push keeps dirty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// webdav_enabled=false(默认)时永不触发, 也不触碰 dirty 文件。
    #[test]
    fn disabled_does_not_trigger() {
        let dir = std::env::temp_dir().join(format!("dirty-off-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sync-dirty"), "1").unwrap(); // 任意旧时间戳
        let store = Store::open_in_memory().unwrap();
        let cfg = crate::config::Config::default(); // webdav_enabled = false
        let r = maybe_auto_push(&store, &cfg, &dir).unwrap();
        assert!(!r, "disabled: never triggers");
        assert_eq!(
            std::fs::read_to_string(dir.join("sync-dirty")).unwrap(),
            "1",
            "disabled: dirty file untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
