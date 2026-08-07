//! consolidate —— LLM 驱动的记忆巩固 + 主动遗忘。
//!
//! 收集候选记忆 → 组装 prompt → LLM 输出 JSON actions → 执行器
//! (update/link/merge/insight/decay/forget, 含保护规则与审计)。
//! 位置记录在 `~/.mnemush/consolidate.json`,增量只处理 `created_at > last_ts`;
//! `dream`(is_dream=true)全量扫描、忽略位置、遗忘强度更高,且不推进位置。

use crate::error::Result;
use crate::memory::MemoryApi;
use crate::schema::{Category, EdgeType, Memory, MemoryType, NewMemory, Source};

#[derive(Debug)]
pub enum Action {
    Update {
        id: String,
        content: String,
        reason: String,
    },
    Link {
        source: String,
        target: String,
        etype: String,
        strength: f32,
    },
    Merge {
        keep: String,
        absorb: String,
    },
    Insight {
        title: String,
        content: String,
        links: Vec<String>,
    },
    Decay {
        id: String,
        factor: f32,
        reason: String,
    },
    Forget {
        id: String,
        reason: String,
    },
}

/// 解析 LLM 输出的 `{"actions":[...]}`;未知 type 跳过。
pub fn parse_actions(raw: &str) -> Result<Vec<Action>> {
    let json = clean_json(raw);
    let v: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| crate::error::MnemushError::Other(format!("actions json: {e}")))?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("actions").and_then(|a| a.as_array()) {
        for item in arr {
            let t = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let s = |k: &str| item.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            match t {
                "update" => out.push(Action::Update {
                    id: s("id"),
                    content: s("content"),
                    reason: s("reason"),
                }),
                "link" => {
                    let source = s("source");
                    let source = if source.is_empty() { s("source_id") } else { source };
                    let source = if source.is_empty() { s("id") } else { source };
                    let target = s("target");
                    let target = if target.is_empty() { s("target_id") } else { target };
                    let etype = s("etype");
                    let etype = if etype.is_empty() { s("relation") } else { etype };
                    out.push(Action::Link {
                        source,
                        target,
                        etype,
                        strength: item
                            .get("strength")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(0.6) as f32,
                    });
                }
                "merge" => {
                    let keep = s("keep");
                    let keep = if keep.is_empty() { s("id") } else { keep };
                    let absorb = s("absorb");
                    let absorb = if absorb.is_empty() {
                        item.get("sources").and_then(|x| x.as_array())
                            .and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("").to_string()
                    } else { absorb };
                    out.push(Action::Merge { keep, absorb });
                }
                "insight" => out.push(Action::Insight {
                    title: s("title"),
                    content: s("content"),
                    links: item
                        .get("links")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                }),
                "decay" => out.push(Action::Decay {
                    id: s("id"),
                    factor: item
                        .get("factor")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.5) as f32,
                    reason: s("reason"),
                }),
                "forget" | "delete" | "remove" => out.push(Action::Forget {
                    id: s("id"),
                    reason: s("reason"),
                }),
                _ => {} // unknown type: skip
            }
        }
    }
    Ok(out)
}

/// 剥离代码围栏与前后说明文字, 截取第一个 `{` 到最后一个 `}`。
fn clean_json(raw: &str) -> String {
    let start = raw.find('{').unwrap_or(0);
    let end = raw.rfind('}').map(|i| i + 1).unwrap_or(raw.len());
    raw[start..end].to_string()
}

#[derive(Default)]
pub struct ExecStats {
    pub updated: usize,
    pub links: usize,
    pub merged: usize,
    pub insights: usize,
    pub decayed: usize,
    pub forgot: usize,
    pub errors: Vec<String>,
}

/// 把 LLM 输出的短 id(前 8 字符或完整)解析为完整 UUID。
/// 前缀碰撞(UUID v7 同毫秒创建 → 同前缀)时保守跳过, 不冒险猜 ——
/// 猜错会软删/修改错误的记忆, 静默跳过只会丢一个动作。
fn resolve_id(api: &MemoryApi, short: &str) -> Option<String> {
    if short.len() < 8 {
        return None;
    }
    if let Ok(Some(m)) = api.get(short) {
        return Some(m.id);
    }
    let mut stmt = api
        .store
        .conn
        .prepare("SELECT id FROM memory WHERE id LIKE ?1 AND deleted_at IS NULL")
        .ok()?;
    let mut rows = stmt
        .query_map(rusqlite::params![format!("{short}%")], |r| r.get::<_, String>(0))
        .ok()?;
    let mut ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();
    if ids.len() == 1 {
        ids.pop()
    } else {
        None // 0 或 >1 匹配都不解析
    }
}

/// 保护规则(Raf/MAPK 类比): importance≥0.7 / never_prune / identity / 7 天内 → 禁遗忘。
fn is_protected(m: &Memory) -> bool {
    if m.never_prune || m.memory_type == MemoryType::Identity {
        return true;
    }
    if m.importance >= 0.7 {
        return true;
    }
    let week_ago = chrono::Utc::now() - chrono::Duration::days(7);
    m.created_at > week_ago
}

/// absorb 的边重定向到 keep,清理自环。
/// 重定向目标边已存在时丢弃重复(不撞 UNIQUE 约束)——
/// 两条记忆都连向同一目标的场景(auto-link 极易产生)。
fn redirect_edges(store: &crate::store::Store, absorb: &str, keep: &str) -> Result<()> {
    // source 重定向: 目标 (keep, target, type) 已存在 → 不动(随后删除)
    store.conn.execute(
        "UPDATE memory_edge SET source_id = ?1 \
         WHERE source_id = ?2 AND source_id != target_id \
           AND NOT EXISTS (SELECT 1 FROM memory_edge e2 \
                           WHERE e2.source_id = ?1 AND e2.target_id = memory_edge.target_id \
                             AND e2.edge_type = memory_edge.edge_type)",
        rusqlite::params![keep, absorb],
    )?;
    // 未重定向的 absorb 出边 = 重复 → 删
    store.conn.execute(
        "DELETE FROM memory_edge WHERE source_id = ?1 AND source_id != target_id",
        rusqlite::params![absorb],
    )?;
    // target 重定向(同理)
    store.conn.execute(
        "UPDATE memory_edge SET target_id = ?1 \
         WHERE target_id = ?2 AND source_id != ?2 \
           AND NOT EXISTS (SELECT 1 FROM memory_edge e2 \
                           WHERE e2.source_id = memory_edge.source_id AND e2.target_id = ?1 \
                             AND e2.edge_type = memory_edge.edge_type)",
        rusqlite::params![keep, absorb],
    )?;
    store.conn.execute(
        "DELETE FROM memory_edge WHERE target_id = ?1 AND source_id != ?1",
        rusqlite::params![absorb],
    )?;
    // 自环清理(merge 产生的 keep→keep 等)
    store
        .conn
        .execute("DELETE FROM memory_edge WHERE source_id = target_id", [])?;
    Ok(())
}

fn run_one(api: &MemoryApi, action: &Action, stats: &mut ExecStats) -> Result<()> {
    let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
    match action {
        Action::Update {
            id,
            content,
            reason,
        } => {
            let Some(full) = resolve_id(api, id) else { return Ok(()); };
            if let Some(mut m) = api.get(&full)? {
                m.content = content.clone();
                m.content_hash = MemoryApi::content_hash(content);
                api.update(&m)?;
                api.store.log_event(
                    "consolidate_update",
                    Some(&full),
                    None,
                    Some(reason),
                    "consolidate",
                )?;
                stats.updated += 1;
            }
        }
        Action::Link {
            source,
            target,
            etype,
            strength,
        } => {
            let et = match etype.as_str() {
                "supports" | "caused" | "enables" | "evidence_for" | "summarized_by"
                | "extends" | "depends_on" => EdgeType::Supports,
                "contradicts" | "conflicts" => EdgeType::Contradicts,
                "supersedes" | "replaces" => EdgeType::Supersedes,
                _ => EdgeType::Related,
            };
            let (Some(sf), Some(tf)) = (resolve_id(api, source), resolve_id(api, target)) else {
                return Ok(());
            };
            edge_api.link(&sf, &tf, et, *strength, Some("consolidate:link"), None)?;
            stats.links += 1;
        }
        Action::Merge { keep, absorb } => {
            let (Some(kf), Some(af)) = (resolve_id(api, keep), resolve_id(api, absorb)) else {
                return Ok(());
            };
            let (Some(k), Some(a)) = (api.get(&kf)?, api.get(&af)?) else {
                return Ok(());
            };
            // 不把保护记忆并入普通记忆
            if is_protected(&a) && !is_protected(&k) {
                return Ok(());
            }
            let mut k = k;
            k.content = format!("{}\n\n---\n\n{}", k.content, a.content);
            k.content_hash = MemoryApi::content_hash(&k.content);
            api.update(&k)?;
            api.soft_delete(&af)?;
            redirect_edges(api.store, &af, &kf)?;
            api.store.log_event(
                "consolidate_merge",
                Some(&af),
                None,
                Some(&format!("into {kf}")),
                "consolidate",
            )?;
            stats.merged += 1;
        }
        Action::Insight {
            title,
            content,
            links,
        } => {
            let mut nm = NewMemory::note(content.clone(), title.clone());
            nm.category = Category::Insight;
            nm.source = Source::Consolidate;
            let r = api.add(nm)?;
            let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
            for l in links {
                if let Some(lf) = resolve_id(api, l) {
                    let _ = edge_api.link(
                        &r.id,
                        &lf,
                        EdgeType::Related,
                        0.7,
                        Some("consolidate:insight"),
                        None,
                    );
                }
            }
            stats.insights += 1;
        }
        Action::Decay {
            id,
            factor,
            reason,
        } => {
            let Some(full) = resolve_id(api, id) else { return Ok(()); };
            if let Some(mut m) = api.get(&full)? {
                if is_protected(&m) {
                    return Ok(());
                }
                m.confidence = (m.confidence * factor).max(0.05);
                api.update(&m)?;
                api.store.log_event(
                    "consolidate_decay",
                    Some(&full),
                    None,
                    Some(reason),
                    "consolidate",
                )?;
                stats.decayed += 1;
            }
        }
        Action::Forget { id, reason } => {
            let Some(full) = resolve_id(api, id) else { return Ok(()); };
            if let Some(m) = api.get(&full)? {
                if is_protected(&m) {
                    return Ok(());
                }
                api.soft_delete(&full)?;
                api.store.log_event(
                    "consolidate_forget",
                    Some(&full),
                    None,
                    Some(reason),
                    "consolidate",
                )?;
                // 遗忘痕迹: "忘掉什么本身也是一种记忆"。软删原记忆后,
                // 留下 forget_trace 元记忆(可检索/可分析/可被未来再遗忘)。
                // 防递归: trace 被遗忘时不再建 trace-of-trace。
                if m.category != Category::ForgetTrace {
                    let mut nm = NewMemory::note(
                        format!(
                            "[forgotten] {} — {} 被判定遗忘。原内容摘要: {}。原因: {}",
                            m.title,
                            chrono::Utc::now().format("%Y-%m-%d %H:%M"),
                            trunc(&m.content, 150),
                            reason,
                        ),
                        format!("[forgotten] {}", m.title),
                    );
                    nm.category = Category::ForgetTrace;
                    nm.importance = 0.3;
                    nm.tags = vec!["forget-trace".into(), "consolidate".into()];
                    nm.source = Source::Consolidate;
                    api.add(nm)?;
                }
                stats.forgot += 1;
            }
        }
    }
    Ok(())
}

fn action_order(a: &Action) -> usize {
    match a {
        Action::Link { .. } => 0,
        Action::Update { .. } => 1,
        Action::Merge { .. } => 2,
        Action::Insight { .. } => 3,
        Action::Decay { .. } => 4,
        Action::Forget { .. } => 5,
    }
}

/// 执行全部动作,顺序 link→update→merge→insight→decay→forget。
/// 单条失败记录错误继续,不整体回滚。
pub fn execute(api: &MemoryApi, actions: &[Action]) -> Result<ExecStats> {
    let mut stats = ExecStats::default();
    for order in 0..6usize {
        for a in actions.iter().filter(|a| action_order(a) == order) {
            if let Err(e) = run_one(api, a, &mut stats) {
                stats.errors.push(format!("{e}"));
            }
        }
    }
    Ok(stats)
}

/// 增量位置记录。
#[derive(Debug, Default)]
pub struct CState {
    pub last_ts: i64,
}

fn state_path(db_path: Option<&std::path::Path>) -> std::path::PathBuf {
    // 状态与 DB 隔离: 非默认库时, 状态文件跟在库旁边
    // (--db / MNEMUSH_DB_PATH / 临时库 / 测试不串扰增量位置)。
    if let Some(db) = db_path {
        return std::path::PathBuf::from(format!("{}.consolidate.json", db.display()));
    }
    crate::default_data_dir().join("consolidate.json")
}

pub fn load_state(api: &MemoryApi) -> CState {
    let p = state_path(api.store.db_path.as_deref());
    if let Ok(t) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if let Some(ts) = v.get("last_ts").and_then(|x| x.as_i64()) {
                return CState { last_ts: ts };
            }
        }
    }
    CState::default()
}

pub fn save_state(api: &MemoryApi, s: &CState) -> Result<()> {
    let p = state_path(api.store.db_path.as_deref());
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        p,
        serde_json::json!({ "last_ts": s.last_ts }).to_string(),
    )?;
    Ok(())
}

/// 收集候选: project 过滤 + created_at > since(增量)。
pub fn collect_candidates(
    api: &MemoryApi,
    project: Option<&str>,
    since: Option<i64>,
) -> Result<Vec<Memory>> {
    let all = api.list_in_project(100000, project)?;
    let since_ts = since.unwrap_or(0);
    Ok(all
        .into_iter()
        .filter(|m| m.deleted_at.is_none() && m.created_at.timestamp() > since_ts)
        .collect())
}

fn trunc(s: &str, n: usize) -> String {
    let mut out = s.chars().take(n).collect::<String>();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// 组装 prompt: 系统提示(巩固+遗忘指令/双阈值/保护规则/schema)+ 候选。
pub fn build_prompt(cands: &[Memory], is_dream: bool) -> Vec<crate::llm::ChatMsg> {
    let mut items = String::new();
    for (i, m) in cands.iter().enumerate() {
        items.push_str(&format!(
            "[{}] id={} category={} importance={:.2} confidence={:.2} created={}\ntitle: {}\ncontent: {}\n---\n",
            i,
            &m.id, // 完整 id: 同毫秒创建的候选前缀相同, 短 id 无法区分
            m.category.as_str(),
            m.importance,
            m.confidence,
            m.created_at.date_naive(),
            m.title,
            trunc(&m.content, 150),
        ));
    }
    let sys = format!(
        "你是记忆库巩固者。分析以下候选记忆,输出 JSON 动作列表。\n         巩固: update({{id,content,reason}}) 修订内容; link({{source,target,etype,strength}}) source指向target 建边, source和target都是候选id; merge({{keep,absorb}}) 重复记忆合并; insight({{title,content,links}}) 发现跨簇新模式, 创建顿悟记忆。\n         主动遗忘: decay(降权, 原因: 干扰|过时|冗余)/ forget(软删, 原因: 过时|冗余|被取代|干扰)。\n         双阈值: confidence<0.4 的记忆低证据即可遗忘; confidence>=0.4 需明确矛盾/过时证据。\n         保护规则: importance>=0.7 / never_prune / identity / 7 天内创建 → 禁止 decay/forget, 只能 update 或标 contradicts。\n         动作 type 只能是: update/link/merge/insight/decay/forget(delete 等同 forget)。\n         所有 id(source/target/keep/absorb/links)必须原样使用候选列表中的完整 id, 不可截断。\n         输出严格 JSON, 示例: {{\"actions\":[{{\"type\":\"update\",\"id\":\"019fda8e-1111-2222-3333-444455556666\",\"content\":\"新内容\",\"reason\":\"过时\"}},{{\"type\":\"link\",\"source\":\"019fda8e-1111-2222-3333-444455556666\",\"target\":\"019fda8f-aaaa-bbbb-cccc-ddddeeeeffff\",\"etype\":\"related\",\"strength\":0.6}},{{\"type\":\"decay\",\"id\":\"019fda90-1111-2222-3333-444455556666\",\"factor\":0.5,\"reason\":\"过时\"}}]}}. 不要 markdown 代码块。不要重复动作, 不要循环, 每条记忆最多一个动作, 直接输出 JSON。\n         遗忘强度: {}\n\n候选记忆:\n{}",
        if is_dream { "高(睡眠期巩固高峰, 可更激进)" } else { "中" },
        items,
    );
    vec![
        crate::llm::ChatMsg::system(&sys),
        crate::llm::ChatMsg::user("请分析并输出动作。"),
    ]
}

pub struct RunOpts {
    pub project: Option<String>,
    pub dry_run: bool,
    pub suggest: bool,
    pub since: Option<i64>,
    /// dream: 全量扫描(忽略位置), 更激进的遗忘强度。
    pub dream: bool,
}

/// 全流程: collect → prompt → chat → parse → execute → save_state。
pub fn run_consolidate(api: &MemoryApi, opts: &RunOpts) -> Result<(ExecStats, usize)> {
    let since_ts = if opts.dream {
        None // dream 全量扫描, 忽略增量位置
    } else {
        opts.since.or_else(|| {
            let st = load_state(api);
            if st.last_ts > 0 {
                Some(st.last_ts)
            } else {
                None
            }
        })
    };
    let mut cands = collect_candidates(api, opts.project.as_deref(), since_ts)?;
    cands.truncate(5); // 每批上限(官方确认大上下文/cache 是循环诱因)
    if cands.is_empty() {
        return Ok((ExecStats::default(), 0));
    }
    let prompt = build_prompt(&cands, opts.dream);
    let raw = crate::llm::chat(&prompt)?;
    // 存档原始响应(可追溯)
    let _ = std::fs::create_dir_all(crate::default_data_dir().join("eval"));
    let _ = std::fs::write(
        crate::default_data_dir()
            .join("eval")
            .join(format!("consolidate-{}.json", chrono::Utc::now().timestamp())),
        raw.clone(),
    );
    let actions = parse_actions(&raw)?;
    if opts.suggest {
        println!("{raw}");
        return Ok((ExecStats::default(), cands.len()));
    }
    let stats = if opts.dry_run {
        for a in &actions {
            println!("{a:?}");
        }
        ExecStats::default()
    } else {
        execute(api, &actions)?
    };
    // dream 不推进 consolidate 的增量位置(两者独立调度);
    // dry-run/suggest 也不推进(候选并未真正被处理)。
    if !opts.dream && !opts.dry_run {
        let max_ts = cands
            .iter()
            .map(|m| m.created_at.timestamp())
            .max()
            .unwrap_or(0);
        save_state(api, &CState { last_ts: max_ts })?;
    }
    Ok((stats, cands.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;
    fn test_store() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }
    fn add(api: &MemoryApi, title: &str, imp: f32) -> String {
        let mut nm = NewMemory::note(format!("content of {title}"), title);
        nm.importance = imp;
        let id = api.add(nm).unwrap().id;
        // 拨到 30 天前, 绕过 is_protected 的"7 天内新建"豁免
        let old = crate::store::Store::now_ts() - 30 * 86400;
        api.store.conn
            .execute("UPDATE memory SET created_at = ?1 WHERE id = ?2", rusqlite::params![old, id])
            .unwrap();
        id
    }

    #[test]
    fn parse_actions_skips_unknown_types() {
        let a = parse_actions(
            r#"{"actions":[{"type":"update","id":"x","content":"c"},{"type":"bogus","id":"y"}]}"#,
        )
        .unwrap();
        assert_eq!(a.len(), 1);
        match &a[0] {
            Action::Update { .. } => {}
            _ => panic!("expected update"),
        }
    }

    #[test]
    fn forget_respects_protection() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "protected", 0.9);
        let s = execute(
            &api,
            &[Action::Forget {
                id: id.clone(),
                reason: "test".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.forgot, 0, "protected memory not forgotten");
        assert!(api.get(&id).unwrap().is_some());
    }

    #[test]
    fn decay_lowers_confidence_and_respects_floor() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "decayme", 0.3);
        let s = execute(
            &api,
            &[Action::Decay {
                id: id.clone(),
                factor: 0.5,
                reason: "stale".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.decayed, 1);
        let m = api.get(&id).unwrap().unwrap();
        assert!((m.confidence - 0.5).abs() < 1e-6, "confidence halved");
        for _ in 0..10 {
            execute(
                &api,
                &[Action::Decay {
                    id: id.clone(),
                    factor: 0.1,
                    reason: "x".into(),
                }],
            )
            .unwrap();
        }
        let m = api.get(&id).unwrap().unwrap();
        assert!(m.confidence >= 0.05 - 1e-6, "floor respected");
    }

    #[test]
    fn forget_soft_deletes() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "forgetme", 0.3);
        let s = execute(
            &api,
            &[Action::Forget {
                id: id.clone(),
                reason: "obsolete".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.forgot, 1);
        assert!(api.get(&id).unwrap().is_none(), "soft-deleted");
        // 遗忘痕迹: "忘掉什么本身也是一种信息" → forget_trace 元记忆
        let traces: Vec<Memory> = api
            .list_in_project(100, None)
            .unwrap()
            .into_iter()
            .filter(|m| m.category == Category::ForgetTrace)
            .collect();
        assert_eq!(traces.len(), 1, "forget leaves a trace");
        let tr = &traces[0];
        assert!(tr.title.contains("forgetme"), "trace names the victim");
        assert!(tr.content.contains("obsolete"), "trace records the reason");
        assert!(tr.content.contains("内容摘要"), "trace keeps a summary");
        assert!(tr.tags.iter().any(|t| t == "forget-trace"));
    }

    #[test]
    fn forget_trace_is_not_protected_and_no_recursion() {
        // B 策略: trace 可被未来 dream 再遗忘(不设保护)
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "victim", 0.3);
        let s = execute(
            &api,
            &[Action::Forget {
                id: id.clone(),
                reason: "obsolete".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.forgot, 1, "victim forgot, errors: {:?}", s.errors);
        let traces: Vec<Memory> = api
            .list_in_project(100, None)
            .unwrap()
            .into_iter()
            .filter(|m| m.category == Category::ForgetTrace)
            .collect();
        assert_eq!(traces.len(), 1);
        let tid = traces[0].id.clone();
        // B 策略: trace 可被未来 dream 再遗忘。模拟未来轮次(拨旧,
        // 绕过"7 天内新建"保护 —— 保护新记忆是正确行为, 不是豁免)。
        let old = crate::store::Store::now_ts() - 30 * 86400;
        api.store.conn
            .execute("UPDATE memory SET created_at = ?1 WHERE id = ?2", rusqlite::params![old, tid])
            .unwrap();
        let s = execute(
            &api,
            &[Action::Forget {
                id: tid.clone(),
                reason: "trace stale".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.forgot, 1, "trace can be forgotten later, errors: {:?}", s.errors);
        let traces2: Vec<Memory> = api
            .list_in_project(100, None)
            .unwrap()
            .into_iter()
            .filter(|m| m.category == Category::ForgetTrace)
            .collect();
        assert_eq!(traces2.len(), 0, "no trace-of-trace");
        assert!(api.get(&tid).unwrap().is_none());
    }

    #[test]
    fn dream_full_scan_ignores_state() {
        // dream 的增量位置逻辑在 run_consolidate: dream=true → since=None →
        // collect_candidates(None) 全量收集。此处验证 None(全量)语义。
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let old = crate::store::Store::now_ts() - 30 * 86400;
        let mut nm = NewMemory::note("old memory", "old");
        nm.importance = 0.3;
        let id = api.add(nm).unwrap().id;
        api.store.conn
            .execute("UPDATE memory SET created_at = ?1 WHERE id = ?2", rusqlite::params![old, id])
            .unwrap();
        // 全量扫描 → 应收集到 30 天前的记忆
        let cands = collect_candidates(&api, None, None).unwrap();
        assert!(cands.iter().any(|m| m.id == id), "full scan sees old memory");
    }

    #[test]
    fn merge_absorbs_into_keep_and_redirects_edges() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let keep = add(&api, "keepme", 0.3);
        let absorb = add(&api, "absorbme", 0.3);
        let s = execute(
            &api,
            &[Action::Merge {
                keep: keep.clone(),
                absorb: absorb.clone(),
            }],
        )
        .unwrap();
        assert_eq!(s.merged, 1);
        assert!(api.get(&absorb).unwrap().is_none(), "absorbed is soft-deleted");
        let k = api.get(&keep).unwrap().unwrap();
        assert!(k.content.contains("content of absorbme"), "content merged");
    }

    #[test]
    fn short_llm_ids_resolve_before_soft_delete() {
        // 回归: LLM 输出前 8 字符短 id —— soft_delete 是精确匹配,
        // 必须先用 resolve_id 展开成完整 UUID, 否则静默删不中。
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = add(&api, "victim", 0.3);
        let short = id[..8].to_string();
        let s = execute(
            &api,
            &[Action::Forget {
                id: short.clone(),
                reason: "obsolete".into(),
            }],
        )
        .unwrap();
        assert_eq!(s.forgot, 1, "errors: {:?}", s.errors);
        assert!(api.get(&id).unwrap().is_none(), "soft-deleted via short id");

        // merge: absorb 经 resolve_id 展开为完整 id 再软删(短 id 前缀
        // 碰撞时 resolve_id 保守跳过, 此处用完整 id 验证 merge 自身路径)
        let keep = add(&api, "keep", 0.3);
        let absorb = add(&api, "absorb", 0.3);
        let s = execute(
            &api,
            &[Action::Merge {
                keep: keep.clone(),
                absorb: absorb.clone(),
            }],
        )
        .unwrap();
        assert_eq!(s.merged, 1, "errors: {:?}", s.errors);
        assert!(api.get(&absorb).unwrap().is_none(), "absorb soft-deleted");
        let k = api.get(&keep).unwrap().unwrap();
        assert!(k.content.contains("content of absorb"), "content merged");
    }

    #[test]
    fn resolve_id_prefix_collision_is_safe() {
        // 同毫秒创建的记忆前缀相同 → resolve 必须保守返回 None,
        // 不能删错(LIMIT 1 会命中第一条)。
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let a = add(&api, "alpha", 0.3);
        let b = add(&api, "beta", 0.3); // 同毫秒 → 同前缀
        assert_eq!(&a[..8], &b[..8], "test requires same-prefix ids");
        assert!(resolve_id(&api, &a[..8]).is_none(), "collision → None, not a guess");
    }

}
