//! concepts —— 概念表(context priming index): 排序 + title 压缩。
//! 给 agent 的唤起索引(知道记忆库有什么可搜), 零 LLM。

use crate::error::Result;
use crate::memory::MemoryApi;
use crate::schema::Memory;

/// One row of the concept table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConceptEntry {
    /// Compressed memory title.
    pub title: String,
    /// Category name (see [`crate::Category::as_str`]).
    pub category: String,
    /// Raw importance (0.0–1.0).
    pub importance: f32,
    /// Composite sort score (importance × recency × access).
    pub score: f32,
}

const TITLE_MAX: usize = 48;
const NOISE_PREFIXES: &[&str] = &[
    "Task: ",
    "Task — ",
    "task: ",
    "你是 mnemush 项目的",
    "你是 mnemush 项目",
    "你是为 mnemush 项目",
    "请",
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
    if out.chars().count() >= TITLE_MAX {
        out = out.chars().take(TITLE_MAX).collect::<String>() + "…";
    }
    out
}

/// 排序分: importance × recency(30 天半衰) × access 提升。
pub fn score(m: &Memory) -> f32 {
    let age_days =
        ((crate::store::Store::now_ts() - m.created_at.timestamp()).max(0) as f32) / 86400.0;
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
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::memory::MemoryApi;
    use crate::schema::NewMemory;
    use crate::store::Store;

    fn test_store() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }

    #[test]
    fn compress_title_strips_prefix_and_truncates() {
        assert_eq!(
            compress_title("Task: You are a delegated subagent running from a fork"),
            "You are a delegated subagent running from a fork…"
        );
        assert_eq!(
            compress_title("你是 mnemush 项目的实现者, 完成 Task 3"),
            "实现者, 完成 Task 3"
        );
        assert_eq!(compress_title("short title"), "short title");
        assert_eq!(compress_title("第一行\n第二行"), "第一行");
        let long = "x".repeat(100);
        assert_eq!(compress_title(&long).chars().count(), 49, "48 chars + …");
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
        api.store
            .conn
            .execute(
                "UPDATE memory SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![crate::store::Store::now_ts() - 100 * 86400, id_b],
            )
            .unwrap();
        let ma = api.get(&id_a).unwrap().unwrap();
        let mb = api.get(&id_b).unwrap().unwrap();
        assert!(score(&ma) > score(&mb), "important+fresh outranks low+old");
    }

    #[test]
    fn concepts_filters_soft_deleted_and_orders() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let mut hi = NewMemory::note("x", "top concept");
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
}
