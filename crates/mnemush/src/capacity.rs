//! capacity —— 记忆容量管理: 摘要入口 / 驱逐 / 冷判定。
use crate::error::Result;
use crate::memory::MemoryApi;
use crate::store::Store;
use std::path::Path;

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
        sync_fts(api, id, &m.content)?;
        return Ok(());
    }
    let cfg = &api.config.capacity;
    let summary = entry_summary(&m.content, cfg.entry_summary_chars);
    let old_content = m.content.clone(); // update 前先取旧全文, FTS 删除按它匹配
    m.content = summary;
    m.context = Some(format!("neuropil:{path}"));
    m.content_hash = MemoryApi::content_hash(&m.content);
    api.update(&m)?;
    // update_memory_tx 不同步 FTS5(已知限制): content 已变, 手动对齐 FTS 行
    sync_fts(api, id, &old_content)?;
    // 删除旧向量(摘要重新 embed 由调用方决定; 这里只清全文级向量)
    api.store.delete_embeddings_for(id)?;
    Ok(())
}

/// 同步 FTS5 行: 先按旧 content 匹配删除, 再显式 rowid INSERT OR REPLACE。
/// update_memory_tx 只更新 memory 表不碰 memory_fts(见 store.rs 注释),
/// 但 search 依赖 memory_fts(旧全文)命中——content 变更后必须手动对齐,
/// 否则已降级记忆仍会被旧全文搜到。
/// 为什么按 content 匹配删除 + 显式 rowid 插入: memory_fts 是独立表, 其 rowid
/// 与 memory.rowid 的"对齐"靠每次软删同事务删 FTS 行维持——但软删最高 rowid
/// 后再新增记忆, FTS 会复用刚释放的 rowid 而 memory 序列前移一位, 对齐即
/// 破坏。此时按 memory.rowid 匹配的 DELETE 成 no-op(旧全文残留可搜到)或
/// 误删他行; 而自增 INSERT(不指定 rowid)在错位下把新行落到 MAX+1, 与
/// memory.rowid 永久错位——search 以 fts.rowid = memory.rowid 关联, 错位行
/// 的摘要入口不可检索, 且会把该记忆的全文错挂到其他 memory 上返回。
/// 因此: 删除仍按旧 content 匹配(不依赖 rowid 对齐; add 按 content_hash
/// 去重, 同一 content 至多一条活跃记忆), 插入显式携带 memory.rowid
/// (参考 cli.rs reindex 的显式 rowid 模式), INSERT OR REPLACE 兜底极端
/// 冲突——孤儿 FTS 行占住该 rowid 时约束失败 → 替换掉孤儿。
fn sync_fts(api: &MemoryApi, id: &str, old_content: &str) -> Result<()> {
    // 单事务: DELETE+INSERT 中途失败不留 FTS 缺行/残留旧全文
    let tx = api.store.conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM memory_fts WHERE content = ?1",
        rusqlite::params![old_content],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO memory_fts(rowid, title, content, context, tags) \
         SELECT rowid, title, content, context, tags FROM memory WHERE id = ?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 活数据估算 MB(驱逐可收敛): 活跃记忆 content 字节 + 活跃记忆的向量字节
/// + 边估算(128B/条) + 8MB 固定开销(FTS 索引/元数据)。
/// 向量只算活跃记忆; 边只算两端都活跃的(驱逐一端后其边不再计入,
/// 否则驱逐后估算不降)。
/// 注意: 这是逻辑大小, 不是磁盘物理大小——WAL + auto_vacuum=0 下 DELETE
/// 不回收页, 物理空间由 VACUUM 处理(见 enforce_capacity)。触发条件用物理
/// 大小(见 db_physical_mb)。
pub fn db_size_mb(store: &Store) -> Result<f64> {
    let live_bytes: i64 = store.conn.query_row(
        "SELECT COALESCE(SUM(LENGTH(content)),0) \
           + COALESCE((SELECT SUM(LENGTH(e.vec)) FROM memory_embedding e \
                        JOIN memory m ON m.id = e.memory_id \
                        WHERE m.deleted_at IS NULL),0) \
         FROM memory WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let edge_bytes: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM memory_edge e \
         WHERE e.deleted_at IS NULL \
           AND EXISTS (SELECT 1 FROM memory m WHERE m.id = e.source_id AND m.deleted_at IS NULL) \
           AND EXISTS (SELECT 1 FROM memory m WHERE m.id = e.target_id AND m.deleted_at IS NULL)",
        [],
        |r| r.get(0),
    )?;
    Ok((live_bytes as f64 + edge_bytes as f64 * 128.0 + 8.0e6) / 1e6)
}

/// 驱逐评分: 价值/成本。低分先驱逐。
/// score = (importance × confidence × 1/(1+age_days)) / (content_bytes + vec~KB + tags*32)
pub fn eviction_score(m: &crate::schema::Memory) -> f32 {
    let age_days = (crate::store::Store::now_ts() - m.created_at.timestamp()) as f32 / 86400.0;
    let value = m.importance * m.confidence * (1.0 / (1.0 + age_days.max(0.0)));
    let cost = m.content.len() as f32 + 1024.0 /* vec ~KB */ + (m.tags.len() * 32) as f32;
    value / cost
}

/// ① 清 wiki 临时索引(可再生): 软删 project='external-wiki' 的记忆。
pub fn evict_wiki_indexes(api: &MemoryApi) -> Result<usize> {
    let ids: Vec<String> = api
        .store
        .conn
        .prepare("SELECT id FROM memory WHERE deleted_at IS NULL AND project='external-wiki'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in &ids {
        api.soft_delete(id)?;
    }
    Ok(ids.len())
}

/// ② 低分 agent 记忆软删(评分低者先驱逐, 达到 batch 即止)。
/// 取全部活跃记忆按评分升序遍历, 避免"只取最新 N 条"的窗口偏差
/// (最新=最低分被排除)与跳过保护时不补位的问题。
pub fn evict_low_value(api: &MemoryApi, batch: usize) -> Result<usize> {
    let all = api.list_in_project(100_000, None)?; // 全部活跃记忆
    let mut scored: Vec<(f32, &crate::schema::Memory)> =
        all.iter().map(|m| (eviction_score(m), m)).collect();
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let week_ago = crate::store::Store::now_ts() - 7 * 86400;
    let mut evicted = 0;
    for (_, m) in scored {
        if m.importance >= 0.7 || m.never_prune || m.memory_type == crate::schema::MemoryType::Identity {
            continue; // 保护规则
        }
        if m.created_at.timestamp() > week_ago {
            continue; // 7 天内新建禁驱逐(与全局保护规则一致)
        }
        api.soft_delete(&m.id)?;
        evicted += 1;
        if evicted >= batch {
            break;
        }
    }
    Ok(evicted)
}

/// DB 物理大小 MB(触发用): PRAGMA page_count × page_size。
/// 与 db_size_mb(逻辑估算)互补——死页(软删后未回收的页)计入物理但不计入
/// 估算, 估算不超限时物理仍可能稳定超限, 所以驱逐链的触发条件看物理大小;
/// 驱逐后 VACUUM 回收死页, 物理必降。
pub fn db_physical_mb(store: &Store) -> Result<f64> {
    let pages: i64 = store.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = store.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(pages as f64 * page_size as f64 / 1e6)
}

/// 容量报告(供 status/日志)。
#[derive(Debug, Default)]
pub struct CapacityReport {
    pub db_mb: f64,
    pub limit_mb: f64,
    pub evicted_wiki: usize,
    pub evicted_low: usize,
    pub degraded: usize,
}

/// add 后触发: 物理超限 → ①清 wiki 索引 → ②估算仍超 → 低分软删 → ③本链有驱逐时 VACUUM 回收物理空间(freelist_count 守卫; 无驱逐不 VACUUM)。
pub fn enforce_capacity(api: &MemoryApi) -> Result<CapacityReport> {
    let limit = api.config.capacity.max_db_mb;
    let mut rep = CapacityReport {
        db_mb: db_size_mb(&api.store)?,
        limit_mb: limit,
        ..Default::default()
    };
    // F3: 触发条件看物理大小(page_count × page_size)——软删只释放逻辑空间,
    // 死页(已软删行/FTS 残留)计入物理但不计入估算, 估算不超限时物理仍可能
    // 稳定超限。收敛仍按估算(驱逐后活数据必降), VACUUM 回收物理。
    if db_physical_mb(&api.store)? <= limit {
        return Ok(rep);
    }
    rep.evicted_wiki = evict_wiki_indexes(api)?;
    rep.db_mb = db_size_mb(&api.store)?;
    if rep.db_mb > limit {
        rep.evicted_low = evict_low_value(api, api.config.capacity.eviction_batch)?;
        rep.db_mb = db_size_mb(&api.store)?;
    }
    // VACUUM 仅在**本链有实际驱逐**时执行, 且 freelist_count > 0(有可回收页
    // 才 VACUUM): 物理超限但估算不超、无墓碑可清时不再每次 add 全量 VACUUM
    // ——不收敛且卡热路径。物理整理留给 dream 或手动 prune。
    // 注意 VACUUM 不能在事务内执行——此链内无活动事务, 可直接执行。
    if rep.evicted_wiki + rep.evicted_low > 0 {
        let freelist: i64 = api.store.conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        if freelist > 0 {
            api.store.conn.execute_batch("VACUUM")?;
        }
    }
    Ok(rep)
}

/// 冷判定: 入口 last_accessed_at > cold_days 且 文件 mtime 未改 > cold_days。
/// m 是调用方已持有的 &Memory(从 list 拿), 不重新 get。
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
        for m in &mems {
            page.push_str(&format!("## {}\n\n源: `{}`\n\n{}\n\n", m.title, neuropil_path(m).unwrap_or(""), m.content));
        }
        let fname = archive_dir.join(format!("{proj}.md"));
        std::fs::write(&fname, page)?;
        stats.merged += mems.len();
    }
    // 2) tar.gz 打包 archive 目录 → archive.tar.gz, 删除原目录
    let tar_path = archive_dir.with_extension("tar.gz");
    let tar_gz = std::fs::File::create(&tar_path)?;
    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::schema::NewMemory;
    use crate::store::Store;
    use rusqlite::params;
    use uuid::Uuid;

    fn test_store() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }

    /// 用真实 add+get 构造 Memory(直接构造太啰嗦, 且需走 dedup)。
    fn mk(api: &MemoryApi, content: &str, title: &str, imp: f32) -> crate::schema::Memory {
        let mut nm = NewMemory::note(content, title);
        nm.importance = imp;
        let id = api.add(nm).unwrap().id;
        api.get(&id).unwrap().unwrap()
    }

    /// 拨旧 30 天: 绕过"7 天内新建禁驱逐"守卫(F4), 测试评分驱逐路径。
    fn age(api: &MemoryApi, id: &str) {
        let old = Store::now_ts() - 30 * 86400;
        api.store
            .conn
            .execute(
                "UPDATE memory SET created_at = ?1 WHERE id = ?2",
                params![old, id],
            )
            .unwrap();
    }

    /// 临时文件库(F3 物理触发测试): 物理大小随内容增长, VACUUM 有真实磁盘页
    /// 可回收(in-memory 库没有落盘页, 物理大小无法按内容放大)。
    fn test_store_file() -> (Store, Config, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mnemush-cap-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("test.db")).unwrap();
        (store, Config::default(), dir)
    }

    #[test]
    fn db_size_mb_estimates_live_bytes() {
        let (store, _cfg) = test_store();
        let mb = db_size_mb(&store).unwrap();
        assert!(mb > 0.0, "in-memory db still reports size");
    }

    #[test]
    fn db_physical_mb_reads_page_count() {
        let (store, _cfg) = test_store();
        let mb = db_physical_mb(&store).unwrap();
        assert!(mb > 0.0, "in-memory db still has pages");
    }

    /// 软删记忆的向量不计入 db_size_mb(否则驱逐 wiki 索引后估算不降)。
    #[test]
    fn db_size_mb_excludes_soft_deleted_vectors() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let id = api.add(NewMemory::note("mem with vec", "t")).unwrap().id;
        api.store
            .conn
            .execute(
                "INSERT INTO memory_embedding (memory_id, model, dim, vec, updated_at) \
                 VALUES (?1, 'test', 4, X'00000000', ?2)",
                params![id, Store::now_ts()],
            )
            .unwrap();
        let before = db_size_mb(&store).unwrap();
        api.soft_delete(&id).unwrap();
        let after = db_size_mb(&store).unwrap();
        assert!(
            after < before,
            "soft-deleted memory's vector must not count: before={before} after={after}"
        );
    }

    /// 边只算两端都活跃的: 软删一端后其边不再计入 db_size_mb(否则驱逐 wiki
    /// 索引后边部分估算不降, 与向量 gap 同类)。
    #[test]
    fn db_size_mb_excludes_edges_of_soft_deleted_endpoints() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        // 两段无共享词, 避免 auto-link 在 add 时建边干扰计数。
        let a = api.add(NewMemory::note("wombat", "t")).unwrap().id;
        let b = api.add(NewMemory::note("quark", "t")).unwrap().id;
        let base = db_size_mb(&store).unwrap();
        api.store
            .conn
            .execute(
                "INSERT INTO memory_edge \
                 (id, source_id, target_id, edge_type, strength, initial_strength, \
                  created_at, deleted_at) \
                 VALUES (?1, ?2, ?3, 'related', 0.5, 0.5, ?4, NULL)",
                params![Uuid::new_v4().to_string(), a, b, Store::now_ts()],
            )
            .unwrap();
        let with_edge = db_size_mb(&store).unwrap();
        assert!(
            with_edge > base,
            "live edge must count: base={base} with_edge={with_edge}"
        );
        api.soft_delete(&a).unwrap();
        let after = db_size_mb(&store).unwrap();
        assert!(
            after < with_edge,
            "edge of soft-deleted endpoint must not count: with_edge={with_edge} after={after}"
        );
    }

    #[test]
    fn eviction_score_prefers_low_value_high_cost() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let m_low = mk(&api, "low value content", "low", 0.1);
        let m_high = mk(&api, "high value content", "high", 0.9);
        assert!(
            eviction_score(&m_low) < eviction_score(&m_high),
            "low importance evicted first"
        );
    }

    #[test]
    fn evict_wiki_indexes_soft_deletes_project() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        api.add(NewMemory::note("wiki1", "w1")).unwrap();
        api.store
            .conn
            .execute("UPDATE memory SET project='external-wiki'", [])
            .unwrap();
        let n = evict_wiki_indexes(&api).unwrap();
        assert!(n >= 1, "wiki indexes evicted");
        let all = api.list_in_project(100, None).unwrap();
        assert!(all.is_empty(), "all wiki project memories soft-deleted");
    }

    #[test]
    fn evict_low_value_spares_protected() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let low = mk(&api, "evict me", "low", 0.1);
        age(&api, &low.id); // 绕过 7 天新建守卫, 测评分驱逐路径
        let important = mk(&api, "keep me important", "imp", 0.9);
        let mut nm = NewMemory::note("never prune me", "np");
        nm.never_prune = true;
        let np_id = api.add(nm).unwrap().id;
        let fresh = mk(&api, "brand new low value", "fresh", 0.1); // 7 天内新建
        let n = evict_low_value(&api, 10).unwrap();
        assert!(n >= 1, "at least the low-value memory evicted");
        assert!(api.get(&low.id).unwrap().is_none(), "low-value gone");
        assert!(api.get(&important.id).unwrap().is_some(), "high importance spared");
        assert!(api.get(&np_id).unwrap().is_some(), "never_prune spared");
        assert!(api.get(&fresh.id).unwrap().is_some(), "7 天内新建禁驱逐 (F4)");
    }

    /// F3: 驱逐链①——物理超限 → 清 wiki 索引 → 估算清完即达标 → 低分不触发,
    /// VACUUM 回收物理(物理必降)。
    /// 触发看物理(page_count × page_size): wiki 内容 ~10MB → 物理(含 FTS 副本
    /// 与倒排索引)≈33MB, 远超 9MB limit; 收敛看估算: 清 wiki 后 = 8MB 固定
    /// 开销 + 活内容(quark×200 ≈ 1.4KB)≈ 8.001MB < 9MB → 低分不触发。
    /// VACUUM 后物理必降(删除的 FTS 行页回收); 软删行按设计保留 30 天回收窗
    /// (recoverable), 其内容仍占页——所以不断言物理回到 limit 以下, 只断言下降。
    #[test]
    fn enforce_capacity_triggers_on_physical_size() {
        let (store, mut cfg, dir) = test_store_file();
        cfg.capacity.max_db_mb = 9.0;
        let api = MemoryApi::new(&store, &cfg);
        // 空格分隔的重复词: 内容 ~10MB, 且 FTS 查询 token 不会超长/超量
        let wiki = mk(&api, &"wombat ".repeat(1_500_000), "wiki idx", 0.1);
        api.store
            .conn
            .execute("UPDATE memory SET project='external-wiki'", [])
            .unwrap();
        let agent = mk(&api, &"quark ".repeat(200), "agent keep", 0.1);
        let physical_before = db_physical_mb(&store).unwrap();
        assert!(
            physical_before > 9.0,
            "physical over limit triggers the chain: {physical_before} MB"
        );
        let rep = enforce_capacity(&api).unwrap();
        assert!(rep.evicted_wiki >= 1, "wiki evicted: {rep:?}");
        assert_eq!(rep.evicted_low, 0, "estimate converged after wiki clear: {rep:?}");
        assert!(
            api.get(&wiki.id).unwrap().is_none(),
            "wiki memory soft-deleted"
        );
        assert!(
            api.get(&agent.id).unwrap().is_some(),
            "low-value agent memory spared (wiki clear already under limit)"
        );
        let physical_after = db_physical_mb(&store).unwrap();
        assert!(
            physical_after < physical_before,
            "VACUUM reclaimed physical: {physical_before} -> {physical_after} MB"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// F3: 驱逐链②——物理超限且无 wiki 可清 → 低分驱逐触发。
    /// limit=0.001 在最小 1 页(默认 4KB)下物理恒超, 直接验证低分分支。
    #[test]
    fn enforce_capacity_evicts_low_value_when_still_over() {
        let (store, mut cfg) = test_store();
        cfg.capacity.max_db_mb = 0.001;
        let api = MemoryApi::new(&store, &cfg);
        let low = mk(&api, "low value first", "low", 0.1);
        age(&api, &low.id); // 绕过 7 天新建守卫
        let keep = mk(&api, "important keep", "imp", 0.9);
        let rep = enforce_capacity(&api).unwrap();
        assert!(rep.evicted_low >= 1, "low value evicted: {rep:?}");
        assert!(api.get(&low.id).unwrap().is_none(), "low-value gone");
        assert!(api.get(&keep.id).unwrap().is_some(), "high importance spared");
    }

    /// R2: 物理超限但无驱逐时不 VACUUM——死页堆叠造成的物理超限(估算已达标)
    /// 不再让每次 add 全量 VACUUM。旧实现物理超限即 VACUUM, 此场景每次 add
    /// 都全表重建、不收敛; 修复后仅本链有驱逐才 VACUUM(freelist 守卫)。
    /// 场景: 10MB 记忆软删(内容页仍占物理, 估算不再计入)→ 物理超限、
    /// 估算达标、无 wiki 可清 → 驱逐 0 → 无 VACUUM → 物理不变。
    #[test]
    fn enforce_capacity_skips_vacuum_without_eviction() {
        let (store, mut cfg, dir) = test_store_file();
        cfg.capacity.max_db_mb = 9.0;
        let api = MemoryApi::new(&store, &cfg);
        // 10MB 记忆: 软删后死页仍占物理, 但估算(活数据)不再计入
        let big = mk(&api, &"badger ".repeat(1_500_000), "big dead", 0.1);
        api.soft_delete(&big.id).unwrap();
        let small = mk(&api, "numbat", "keep", 0.9);
        let physical_before = db_physical_mb(&store).unwrap();
        assert!(
            physical_before > 9.0,
            "dead pages keep physical over limit: {physical_before} MB"
        );
        let rep = enforce_capacity(&api).unwrap();
        assert_eq!(rep.evicted_wiki, 0, "no wiki to clear: {rep:?}");
        assert_eq!(
            rep.evicted_low, 0,
            "estimate under limit, no low-value eviction: {rep:?}"
        );
        let physical_after = db_physical_mb(&store).unwrap();
        assert_eq!(
            physical_after, physical_before,
            "no eviction -> no VACUUM -> physical untouched: {physical_before} -> {physical_after} MB"
        );
        assert!(api.get(&small.id).unwrap().is_some(), "live memory intact");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
        let id = api.add(NewMemory::note("完整概念的第一句。第二句在这里。第三句是多余的长尾。", "概念")).unwrap().id;
        degrade_to_entry(&api, &id, "neuropils/concepts/概念.md").unwrap();
        let m = api.get(&id).unwrap().unwrap();
        // brief 原断言 is_empty 与 entry_summary 规则矛盾(无标点/短文摘要=全文): 摘要存入 content, 此处断言降级为前 2 句
        assert_eq!(m.content, "完整概念的第一句。第二句在这里。", "full text replaced by 2-sentence summary");
        assert_eq!(m.context.as_deref(), Some("neuropil:neuropils/concepts/概念.md"));
        assert!(m.title.contains("概念"), "title kept");
    }

    /// FTS 同步: degrade 后 search 不再命中旧全文; restore 后恢复命中。
    /// 用短内容使摘要(前 2 句)不含搜索词, 否则命中来自摘要而非旧全文。
    /// 冷判定: 双条件缺一不冷(文件新鲜不冷 / 入口新鲜不冷 / 双旧才冷),
    /// 文件不存在 → 不判冷(避免误归档)。
    #[test]
    fn cold_requires_both_conditions() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let tmp = std::env::temp_dir().join(format!("np-cold-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("old.md");
        std::fs::write(&f, "x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86400);
        let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        drop(fh);
        let mut nm = NewMemory::note("cold content", "cold");
        nm.context = Some(format!("neuropil:{}", f.file_name().unwrap().to_string_lossy()));
        let id = api.add(nm).unwrap().id;
        api.store
            .conn
            .execute(
                "UPDATE memory SET last_accessed_at=?1 WHERE id=?2",
                params![Store::now_ts() - 40 * 86400, id],
            )
            .unwrap();
        let mem = api.get(&id).unwrap().unwrap();
        assert!(is_cold(&api, &mem, &tmp), "cold when both stale");
        // 文件新鲜 → 不冷
        std::fs::write(&f, "fresh").unwrap();
        assert!(!is_cold(&api, &mem, &tmp), "fresh file not cold");
        // 恢复旧 mtime, 改入口新鲜 → 不冷
        let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        fh.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        drop(fh);
        api.store
            .conn
            .execute(
                "UPDATE memory SET last_accessed_at=?1 WHERE id=?2",
                params![Store::now_ts(), id],
            )
            .unwrap();
        let mem = api.get(&id).unwrap().unwrap();
        assert!(!is_cold(&api, &mem, &tmp), "fresh entry not cold");
        // 文件不存在 → 不判冷
        std::fs::remove_file(&f).unwrap();
        assert!(!is_cold(&api, &mem, &tmp), "missing file not cold");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// 压缩: 冷入口 → archive/<proj>.md 归档页 → archive.tar.gz 打包,
    /// 原归档 md 删除(内容进 tar.gz)。
    #[test]
    fn compress_neuropil_archives_and_packs() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let tmp = std::env::temp_dir().join(format!("np-compress-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86400);
        for (name, proj, body) in [
            ("alpha.md", "alpha", "alpha body"),
            ("beta.md", "beta", "beta body"),
        ] {
            let f = tmp.join(name);
            std::fs::write(&f, body).unwrap();
            let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
            fh.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
            drop(fh);
            let mut nm = NewMemory::note(body, name);
            nm.project = Some(proj.to_string());
            nm.context = Some(format!("neuropil:{name}"));
            let id = api.add(nm).unwrap().id;
            api.store
                .conn
                .execute(
                    "UPDATE memory SET last_accessed_at=?1 WHERE id=?2",
                    params![Store::now_ts() - 40 * 86400, id],
                )
                .unwrap();
        }
        let stats = compress_neuropil(&api, &tmp).unwrap();
        assert_eq!(stats.merged, 2, "two entries merged");
        assert_eq!(stats.archived, 2, "two entries archived");
        // 归档页已打包进 tar.gz
        let tar_path = tmp.join("archive.tar.gz");
        assert!(tar_path.exists(), "archive.tar.gz exists");
        let gz = std::fs::File::open(&tar_path).unwrap();
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(gz));
        let names: Vec<String> = ar
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
            .collect();
        assert!(names.contains(&"alpha.md".to_string()), "alpha page packed: {names:?}");
        assert!(names.contains(&"beta.md".to_string()), "beta page packed: {names:?}");
        // 原归档 md 已删除(保留 tar.gz)
        assert!(!tmp.join("archive/alpha.md").exists(), "archive md removed");
        assert!(!tmp.join("archive/beta.md").exists(), "archive md removed");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn degrade_syncs_fts_search() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        // 搜索词 zebraunique 在第 3 句: 摘要=前 2 句, 不含它。
        let id = api
            .add(NewMemory::note("first sentence. second sentence. zebraunique tail", "fts"))
            .unwrap()
            .id;
        let hit = |api: &MemoryApi| {
            api.search("zebraunique", crate::schema::SearchOpts::default())
                .unwrap()
                .iter()
                .any(|h| h.memory.id == id)
        };
        assert!(hit(&api), "old fulltext searchable before degrade");
        degrade_to_entry(&api, &id, "p.md").unwrap();
        assert!(!hit(&api), "degraded entry not hit by old fulltext");
    }

    /// R1 回归: FTS rowid 错位下 sync_fts 按 content 删旧行 + 显式 rowid
    /// INSERT OR REPLACE 重建, 使 search 路由恢复正确。
    /// 错位制造: 软删最高 rowid 记忆 → FTS 复用其刚释放的 rowid, 而 memory 表
    /// 软删行仍占位 → 此后新增记忆的 FTS rowid ≠ memory rowid。旧实现按
    /// memory.rowid 匹配的 DELETE 成 no-op(旧全文残留)或误删他行, 显式 rowid
    /// INSERT 依赖错位下的对齐; 上一轮修复(自增 INSERT)则把新行落 MAX+1,
    /// 与 memory.rowid 永久错位——search 以 fts.rowid = memory.rowid 关联,
    /// 会把 mis 的内容错挂到 other 上返回(摘要入口不可检索 + 错位级联)。
    /// 本修复: content 匹配删除保留, INSERT 显式携带 memory.rowid + OR REPLACE
    /// ——mis 行回到自己的 rowid 槽(search 命中 mis 本体); 错位态下挂在
    /// mis 槽上的 other 行被顶替(它本就错位, 搜索 other 的词只会错挂 mis),
    /// other 在自身下次 sync 时按自己的 rowid 重建。
    /// 断言分两层: 直查 memory_fts 证明"旧全文无残留/行重建", search 路由
    /// 断言(search("mis特有词") 命中 mis 且不得返回 other)是本次补的关键——
    /// 旧实现下 search 返回 other 的内容, 该断言失败, 恰证破坏。
    /// 各条内容用互不相交的词集: 避免 auto-merge(高 Jaccard 近重复 note 会
    /// 软删旧条)干扰 rowid 布局; 搜索词都在第 3 句(摘要=前 2 句, 不含它)。
    #[test]
    fn sync_fts_after_highest_rowid_soft_delete() {
        let (store, mut cfg) = test_store();
        // 关闭 spreading activation(max_neighbor_hops=0): search 只返回 FTS
        // 路由命中——auto-link 边会把 1 跳邻居(other/keep)拉进结果集,
        // 干扰"search(mis 特有词) 不得命中 other"的错位断言。
        cfg.edges.max_neighbor_hops = 0;
        let api = MemoryApi::new(&store, &cfg);
        let keep_id = api
            .add(NewMemory::note("marsupial sleeps. wombat digs burrow. platypusx tail", "keep"))
            .unwrap()
            .id;
        // 软删最高 rowid 记忆 → 破坏 FTS/memory rowid 对齐
        let victim_id = api.add(NewMemory::note("victim body", "victim")).unwrap().id;
        api.soft_delete(&victim_id).unwrap();
        // 错位后新增两条: FTS rowid 复用释放槽位, 与各自 memory rowid 不等
        let mis_id = api
            .add(NewMemory::note("small wallaby hops. forest floor grazes. quokka unique tail", "mis"))
            .unwrap()
            .id;
        let other_id = api
            .add(NewMemory::note("spiky anteater forages. nests under rocks. echidna unique tail", "other"))
            .unwrap()
            .id;
        let fts_rows_with = |api: &MemoryApi, term: &str| -> i64 {
            api.store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_fts WHERE content LIKE ?1",
                    rusqlite::params![format!("%{term}%")],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let hit = |api: &MemoryApi, id: &str, term: &str| {
            api.search(term, crate::schema::SearchOpts::default())
                .unwrap()
                .iter()
                .any(|h| h.memory.id == id)
        };
        // 前置: 错位行的旧全文仍在 memory_fts(就是它让"按 rowid 删"失效)
        assert_eq!(fts_rows_with(&api, "quokka"), 1, "old fulltext row present before degrade");
        assert!(hit(&api, &keep_id, "platypusx"), "aligned memory searchable");
        degrade_to_entry(&api, &mis_id, "p.md").unwrap();
        // 修复后: 旧全文行按 content 匹配删除, 不再残留
        assert_eq!(fts_rows_with(&api, "quokka"), 0, "old fulltext row removed (R1)");
        assert!(
            !hit(&api, &mis_id, "quokka"),
            "degraded summary (前 2 句) has no term"
        );
        // 错位态下 other 的 FTS 行挂在 mis 的 rowid 槽: 显式 rowid 重建会将其
        // 顶替——other 自身下次 sync 时按自己的 rowid 重建(其行本就错位,
        // 搜索 other 的词只会错挂到 mis)。
        assert_eq!(fts_rows_with(&api, "echidna"), 0, "misplaced other row displaced by re-alignment");
        assert!(
            hit(&api, &keep_id, "platypusx"),
            "unrelated memory still searchable (no collateral delete)"
        );
        // 同路径直接触发 sync_fts(恢复全文的场景): 旧摘要行按 content 删,
        // 新全文行显式 rowid 重建。
        let mut m = api.get(&mis_id).unwrap().unwrap();
        let summary = m.content.clone();
        m.content = "small wallaby hops. forest floor grazes. quokka unique tail".to_string();
        api.update(&m).unwrap();
        sync_fts(&api, &mis_id, &summary).unwrap();
        assert_eq!(fts_rows_with(&api, "quokka"), 1, "restored fulltext row rebuilt");
        // R1 search 路由断言: mis 特有词必须命中 mis 本体, 且不得返回 other
        // 的内容(旧实现自增 INSERT 把行落 MAX+1, search 错挂到 other——
        // 此断言在旧实现下失败, 恰证破坏)。
        assert!(hit(&api, &mis_id, "quokka"), "mis searchable via own rowid (R1)");
        assert!(
            !hit(&api, &other_id, "quokka"),
            "mis's term must not surface other's memory (R1)"
        );
    }
}
