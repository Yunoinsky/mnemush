//! neuropils —— 文件树内容层(文件=源, mnemush=mushroom_body 索引)。
//!
//! 记忆以 markdown 文件树为权威源(Agent 可用 grep/cat/tree 直接读),
//! `import-tree` 增量同步进 SQLite(mushroom_body:FTS + 向量 + 边)。
//! 链接格式:frontmatter `links:` 列表 + 正文 `[[target]]` 或
//! wiki 风格 `[label](path/ID)`(external-wiki 兼容)。

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::error::Result;
use crate::memory::MemoryApi;
use crate::schema::{Category, NewMemory, Source};

/// 默认 neuropil 的项目隔离名(任意目录树可通过 `--project` 成为独立 neuropil)。
pub const PROJECT: &str = "neuropils";
/// 记忆上标记来源文件路径的 tag 前缀,用于审计与增量同步。
pub const NEUROPIL_TAG_PREFIX: &str = "neuropil-path:";

/// 一个记忆文件(解析后的表示)。
pub struct MemoryFile {
    pub path: PathBuf,
    pub title: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub content: String,
    pub hash: String,
}

/// 极简 frontmatter 解析:`---\nkey: value\n---`。返回 (键值对, 正文起始偏移)。
fn parse_frontmatter(text: &str) -> Option<(Vec<(String, String)>, usize)> {
    if !text.starts_with("---\n") {
        return None;
    }
    let end = text[4..].find("\n---").map(|i| i + 4)?;
    let block = &text[4..end];
    let mut kv = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            kv.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some((kv, end + 4))
}

/// 解析 YAML 风格列表 `[a, b]` 或逗号分隔 `a, b`。
fn parse_list(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = if v.starts_with('[') && v.ends_with(']') {
        &v[1..v.len() - 1]
    } else {
        v
    };
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 解析一个记忆文件;非 `.md` 返回 `None`。无 frontmatter 时用文件名作 title。
pub fn parse_file(path: &Path) -> Result<Option<MemoryFile>> {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let (kv, body_start) = match parse_frontmatter(&text) {
        Some(x) => x,
        None => {
            // 无 frontmatter: 整个文件作为内容, 文件名作 title。
            let title = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let content = text.trim_start().to_string();
            return Ok(Some(MemoryFile {
                path: path.to_path_buf(),
                title,
                category: None,
                tags: vec![],
                links: vec![],
                content: content.clone(),
                hash: MemoryApi::content_hash(&content),
            }));
        }
    };
    let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    let title = get("title").unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let content = text[body_start..].trim_start().to_string();
    let hash = MemoryApi::content_hash(&content);
    Ok(Some(MemoryFile {
        path: path.to_path_buf(),
        title,
        category: get("category"),
        tags: get("tags").map(|v| parse_list(&v)).unwrap_or_default(),
        links: get("links").map(|v| parse_list(&v)).unwrap_or_default(),
        content,
        hash,
    }))
}

/// import-tree 的统计结果。
pub struct ImportStats {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub edges: usize,
}

/// 递归收集目录下所有 `.md` 文件(排序保证确定性)。
fn collect_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in std::fs::read_dir(d)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)?;
            } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
                out.push(p);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// 增量同步:文件树 → mushroom_body 索引(project 隔离)。
/// 按 title join 现有记忆:hash 相同跳过,不同则 update(保留 id),
/// 不存在则 add。随后两遍建边:显式 links + 正文 wikilink → related/supports,
/// 同目录共现 → weak related。返回各项统计。
pub fn import_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ImportStats> {
    let mut existing: std::collections::HashMap<String, crate::schema::Memory> =
        std::collections::HashMap::new();
    for m in api.list_in_project(100000, Some(project))? {
        existing.insert(m.title.clone(), m);
    }
    let mut stats = ImportStats {
        added: 0,
        updated: 0,
        skipped: 0,
        edges: 0,
    };
    // 第一遍: 解析文件 → (MemoryFile, memory_id), 增量 add/update。
    let files = collect_md_files(dir)?;
    let mut parsed: Vec<(MemoryFile, String)> = Vec::with_capacity(files.len());
    let mut id_by_title: std::collections::HashMap<String, String> = existing
        .iter()
        .map(|(t, m)| (t.clone(), m.id.clone()))
        .collect();
    for path in files {
        let mf = match parse_file(&path)? {
            Some(m) => m,
            None => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let id = match existing.get(&mf.title) {
            Some(old) if old.content_hash == mf.hash => {
                stats.skipped += 1;
                old.id.clone()
            }
            Some(old) => {
                let mut m = old.clone();
                m.content = mf.content.clone();
                // update_memory_tx 不重算 hash —— 必须手动更新,否则下次 import 误判为未变
                m.content_hash = MemoryApi::content_hash(&m.content);
                m.category = mf
                    .category
                    .as_deref()
                    .and_then(Category::parse)
                    .unwrap_or(Category::Note);
                m.tags = mf.tags.clone();
                if !m.tags.iter().any(|t| t.starts_with(NEUROPIL_TAG_PREFIX)) {
                    m.tags.push(format!("{NEUROPIL_TAG_PREFIX}{rel}"));
                }
                let id = m.id.clone();
                api.update(&m)?;
                stats.updated += 1;
                id
            }
            None => {
                let mut nm = NewMemory::note(mf.content.clone(), mf.title.clone());
                nm.category = mf
                    .category
                    .as_deref()
                    .and_then(Category::parse)
                    .unwrap_or(Category::Note);
                nm.tags = {
                    let mut t = mf.tags.clone();
                    t.push(format!("{NEUROPIL_TAG_PREFIX}{rel}"));
                    t
                };
                nm.project = Some(project.to_string());
                nm.source = Source::FileTree;
                let id = api.add(nm)?.id;
                stats.added += 1;
                id
            }
        };
        id_by_title.insert(mf.title.clone(), id.clone());
        parsed.push((mf, id));
    }

    // 第二遍: 建边(mushroom_body 跨簇连接)。
    let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
    for (mf, id) in &parsed {
        let mut targets = mf.links.clone();
        targets.extend(inline_wikilinks(&mf.content));
        for t in targets {
            let Some(tid) = resolve_target(&parsed, &id_by_title, &t) else {
                continue; // 目标缺失, 跳过
            };
            if tid == *id {
                continue; // 自环
            }
            let etype = if t.trim_start().starts_with("supports:") {
                crate::schema::EdgeType::Supports
            } else {
                crate::schema::EdgeType::Related
            };
            edge_api.link(id, &tid, etype, 0.6, Some("neuropil:wikilink"), None)?;
            stats.edges += 1;
        }
    }
    // 同目录共现: 两两 weak related(provenance=neuropil:copath)。
    let mut by_dir: std::collections::HashMap<String, Vec<&(MemoryFile, String)>> =
        std::collections::HashMap::new();
    for item in &parsed {
        let d = item
            .0
            .path
            .parent()
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .to_string();
        by_dir.entry(d).or_default().push(item);
    }
    for (_d, group) in by_dir {
        if group.len() < 2 {
            continue;
        }
        for i in 0..group.len() {
            for j in (i + 1)..group.len() {
                edge_api.link(
                    &group[i].1,
                    &group[j].1,
                    crate::schema::EdgeType::Related,
                    0.3,
                    Some("neuropil:copath"),
                    None,
                )?;
                stats.edges += 1;
            }
        }
    }
    Ok(stats)
}

/// 提取正文中的链接: `[[target]]` 与 wiki 风格 `[label](path/ID)`。
fn inline_wikilinks(content: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]|\[[^\]]*\]\(([^)]+)\)").unwrap();
    re.captures_iter(content)
        .map(|c| {
            c.get(1)
                .map(|m| m.as_str().trim().to_string())
                .or_else(|| c.get(2).map(|m| m.as_str().trim().to_string()))
                .unwrap_or_default()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把链接目标解析为记忆 id: 优先按路径后缀匹配(含 wiki 风格 `papers/X`,
/// 容忍 `.md` 扩展名差异),其次按 title 匹配。
fn resolve_target(
    parsed: &[(MemoryFile, String)],
    id_by_title: &std::collections::HashMap<String, String>,
    target: &str,
) -> Option<String> {
    let t = target.trim_start_matches("./");
    let t_norm = t.trim_end_matches(".md");
    // 路径分隔符在 Windows 上是 `\\`,markdown 链接里通常写 `/`——直接
    // ends_with 会错过。统一转成 `/` 再比对,跨平台一致。
    if let Some((_, id)) = parsed.iter().find(|(f, _)| {
        let p_slash = f.path.to_string_lossy().replace('\\', "/");
        let t_slash = t.replace('\\', "/");
        let t_norm_slash = t_norm.replace('\\', "/");
        p_slash.ends_with(&t_slash) || p_slash.trim_end_matches(".md").ends_with(&t_norm_slash)
    }) {
        return Some(id.clone());
    }
    id_by_title.get(target.trim()).cloned()
}

/// export-tree 的统计结果。
pub struct ExportStats {
    pub written: usize,
}

/// 把项目记忆落盘为 markdown 文件树(`<category>/<title>.md`)。
/// frontmatter 含 title/category/tags;正文为记忆内容。
pub fn export_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ExportStats> {
    let mems = api.list_in_project(100000, Some(project))?;
    let mut stats = ExportStats { written: 0 };
    for m in mems {
        if m.deleted_at.is_some() {
            continue;
        }
        let cat_dir = dir.join(m.category.as_str());
        std::fs::create_dir_all(&cat_dir)?;
        let fname = format!("{}.md", sanitize_filename(&m.title));
        let tags = if m.tags.is_empty() {
            String::new()
        } else {
            format!("tags: [{}]\n", m.tags.join(", "))
        };
        let fm = format!(
            "---\ntitle: {}\ncategory: {}\n{}---\n\n{}\n",
            m.title,
            m.category.as_str(),
            tags,
            m.content
        );
        std::fs::write(cat_dir.join(&fname), fm)?;
        stats.written += 1;
    }
    Ok(stats)
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c.is_whitespace() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::config::Config;
    use crate::schema::{Category, NewMemory, SearchOpts, Source};
    use crate::store::Store;

    fn test_store() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }

    fn tmp_file(name: &str, text: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn parses_full_frontmatter() {
        let p = tmp_file(
            "a.md",
            "---\ntitle: GitHub proxy setup\ncategory: lesson\ntags: [proxy, github]\nlinks: [\"b.md\", \"other-title\"]\n---\n# Body\nAccessing GitHub needs Clash 7890.\n",
        );
        let mf = parse_file(&p).unwrap().expect("parses");
        assert_eq!(mf.title, "GitHub proxy setup");
        assert_eq!(mf.category.as_deref(), Some("lesson"));
        assert_eq!(mf.tags, vec!["proxy", "github"]);
        assert_eq!(mf.links, vec!["b.md", "other-title"]);
        assert!(mf.content.contains("Accessing GitHub"));
        assert!(!mf.content.contains("---"));
        assert!(!mf.hash.is_empty());
    }

    #[test]
    fn no_frontmatter_uses_filename_title() {
        let p = tmp_file("fallback.md", "# Just a heading\nsome body text here\n");
        let mf = parse_file(&p).unwrap().expect("parses");
        assert_eq!(mf.title, "fallback");
        assert!(mf.category.is_none());
    }

    #[test]
    fn non_md_returns_none() {
        let p = tmp_file("notes.txt", "not markdown");
        assert!(parse_file(&p).unwrap().is_none());
    }

    fn tmp_tree(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("neuropil-import-{}-{}", std::process::id(), name));
        fs::create_dir_all(d.join("lesson/proxy")).unwrap();
        fs::create_dir_all(d.join("decision/rename")).unwrap();
        fs::write(
            d.join("lesson/proxy/clash.md"),
            "---\ntitle: Clash port 7890\ncategory: lesson\ntags: [proxy]\n---\nThe local Clash proxy listens on 127.0.0.1:7890.\n",
        )
        .unwrap();
        fs::write(
            d.join("decision/rename/rename.md"),
            "---\ntitle: Project rename\ncategory: decision\n---\nmneme was renamed to mnemush.\n",
        )
        .unwrap();
        d
    }

    #[test]
    fn import_creates_memories_and_is_idempotent() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree("create");
        let s1 = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s1.added, 2);
        let s2 = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s2.added, 0);
        assert_eq!(s2.skipped, 2);
        let hits = api
            .search(
                "clash 7890",
                SearchOpts {
                    limit: Some(5),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(hits.iter().any(|h| h.memory.title == "Clash port 7890"));
    }

    #[test]
    fn import_updates_edited_file_in_place() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree("update");
        import_tree(&api, &dir, PROJECT).unwrap();
        let p = dir.join("lesson/proxy/clash.md");
        fs::write(
            &p,
            "---\ntitle: Clash port 7890\ncategory: lesson\n---\nNow with a new detail: socks5 too.\n",
        )
        .unwrap();
        let s = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s.updated, 1);
        assert_eq!(s.added, 0);
        let mems = api.list_in_project(100, Some(PROJECT)).unwrap();
        assert_eq!(mems.len(), 2, "edit updates, does not duplicate");
        let clash = mems.iter().find(|m| m.title == "Clash port 7890").unwrap();
        assert!(clash.content.contains("socks5"));
    }

    fn tmp_tree_linked() -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-link-{}", std::process::id()));
        fs::create_dir_all(d.join("a")).unwrap();
        fs::create_dir_all(d.join("b")).unwrap();
        fs::write(
            d.join("a/x.md"),
            "---\ntitle: Alpha\ntags: [t1]\nlinks: [\"b/y.md\"]\n---\nAlpha content about thing one.\n",
        )
        .unwrap();
        fs::write(
            d.join("b/y.md"),
            "---\ntitle: Beta\ntags: [t2]\n---\nBeta content mentions [[Alpha]] inline.\n",
        )
        .unwrap();
        d
    }

    #[test]
    fn wikilinks_create_edges_between_memories() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree_linked();
        let s = import_tree(&api, &dir, PROJECT).unwrap();
        assert!(s.edges >= 1, "frontmatter link becomes an edge");
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let mems = api.list_in_project(100, Some(PROJECT)).unwrap();
        let alpha = mems.iter().find(|m| m.title == "Alpha").unwrap();
        let beta = mems.iter().find(|m| m.title == "Beta").unwrap();
        let neigh: Vec<String> = eapi
            .neighbors(&alpha.id, 1)
            .unwrap()
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        assert!(
            neigh.contains(&beta.id),
            "Alpha linked to Beta via frontmatter links"
        );
        let neigh2: Vec<String> = eapi
            .neighbors(&beta.id, 1)
            .unwrap()
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        assert!(
            neigh2.contains(&alpha.id),
            "Beta linked to Alpha via inline [[Alpha]]"
        );
    }

    fn tmp_tree_wiki_style() -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-wiki-{}", std::process::id()));
        fs::create_dir_all(d.join("concepts")).unwrap();
        fs::create_dir_all(d.join("papers")).unwrap();
        fs::write(
            d.join("concepts/sleep.md"),
            "---\ntitle: Sleep\n---\nA state studied in [Drosophila sleep paper](papers/AAA111).\n",
        )
        .unwrap();
        fs::write(
            d.join("papers/AAA111.md"),
            "---\ntitle: Drosophila sleep paper\n---\nStudy on sleep in fruit flies.\n",
        )
        .unwrap();
        d
    }

    #[test]
    fn wiki_style_links_create_edges() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree_wiki_style();
        import_tree(&api, &dir, "external-wiki-sample").unwrap();
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let mems = api
            .list_in_project(100, Some("external-wiki-sample"))
            .unwrap();
        let sleep = mems.iter().find(|m| m.title == "Sleep").unwrap();
        let paper = mems
            .iter()
            .find(|m| m.title == "Drosophila sleep paper")
            .unwrap();
        let neigh: Vec<String> = eapi
            .neighbors(&sleep.id, 1)
            .unwrap()
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        assert!(
            neigh.contains(&paper.id),
            "wiki-style [label](papers/X) link becomes an edge"
        );
        // 确认这条边是 wikilink 来源(auto-link 也可能连上, 需 provenance 区分)
        let has_wikilink = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_edge WHERE source_id=?1 AND target_id=?2 AND provenance='neuropil:wikilink'",
                rusqlite::params![sleep.id, paper.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(has_wikilink > 0, "edge provenance is neuropil:wikilink");
    }

    #[test]
    fn same_directory_files_get_weak_edges() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = std::env::temp_dir().join(format!("neuropil-copath-{}", std::process::id()));
        fs::create_dir_all(dir.join("topic1")).unwrap();
        fs::write(
            dir.join("topic1/m1.md"),
            "---\ntitle: M1\n---\nContent of m1.\n",
        )
        .unwrap();
        fs::write(
            dir.join("topic1/m2.md"),
            "---\ntitle: M2\n---\nContent of m2.\n",
        )
        .unwrap();
        fs::write(
            dir.join("topic1/m3.md"),
            "---\ntitle: M3\n---\nContent of m3.\n",
        )
        .unwrap();
        fs::write(
            dir.join("other.md"),
            "---\ntitle: Other\n---\nUnrelated content elsewhere.\n",
        )
        .unwrap();
        import_tree(&api, &dir, PROJECT).unwrap();
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let mems = api.list_in_project(100, Some(PROJECT)).unwrap();
        let m1 = mems.iter().find(|m| m.title == "M1").unwrap();
        let siblings: Vec<String> = mems
            .iter()
            .filter(|m| m.title.starts_with('M') && m.id != m1.id)
            .map(|m| m.id.clone())
            .collect();
        let neighbors: Vec<String> = eapi
            .neighbors(&m1.id, 1)
            .unwrap()
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        for sid in &siblings {
            assert!(neighbors.contains(sid), "M1 linked to sibling in same dir");
        }
        // 跨目录的 Other 不应被 copath 连到 M1(除非内容相似触发 auto-link)。
        let other = mems.iter().find(|m| m.title == "Other").unwrap();
        let other_neigh: Vec<String> = eapi
            .neighbors(&other.id, 1)
            .unwrap()
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        assert!(
            !other_neigh.contains(&m1.id),
            "cross-directory copath edge should not exist"
        );
    }

    #[test]
    fn export_then_import_roundtrip() {
        let (store, cfg) = test_store();
        let api = MemoryApi::new(&store, &cfg);
        let mut nm = NewMemory::note("Content about export roundtrip.", "Export Me");
        nm.category = Category::Lesson;
        nm.tags = vec!["x".into()];
        nm.project = Some(PROJECT.to_string());
        nm.source = Source::FileTree;
        api.add(nm).unwrap();
        let out = std::env::temp_dir().join(format!("neuropil-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        let s = export_tree(&api, &out, PROJECT).unwrap();
        assert_eq!(s.written, 1);
        let f = out.join("lesson").join("Export Me.md");
        assert!(f.exists(), "file written under category dir");
        let text = fs::read_to_string(&f).unwrap();
        assert!(text.contains("title: Export Me"));
        assert!(text.contains("category: lesson"));
        assert!(text.contains("Content about export roundtrip."));
        // import back — hash identical → skipped
        let api2 = MemoryApi::new(&store, &cfg);
        let s2 = import_tree(&api2, &out, PROJECT).unwrap();
        assert_eq!(s2.skipped, 1, "roundtrip is stable");
    }
}
