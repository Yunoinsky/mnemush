# 文件树记忆(文件=源, mnemush=关系层) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 mnemush 支持"neuropils(文件树内容层)为源、mushroom_body(图索引层)维护非树状关系"的记忆模式,新增 `import-tree` / `export-tree` 命令。

**Architecture:** 新增 `neuropils.rs` 模块(极简 frontmatter 解析 + 目录树遍历 + 增量同步 + wikilink 边 + 导出),CLI 挂两个子命令。复用现有 `MemoryApi::add/update`(自带 dedup/auto-link/向量写入)与 `EdgeApi::link`(幂等建边)。文件为权威源,SQLite(mushroom_body)是索引;`add`/`search`/MCP 全不动(双轨共存)。

**Tech Stack:** Rust(crates/mnemush,无新依赖——frontmatter 用手写极简解析器,不引 serde_yaml)、rusqlite、regex。

**Spec:** `docs/superpowers/specs/2026-08-07-file-tree-memory-design.md`

## Global Constraints

- 无新 crate 依赖(frontmatter 解析手写,`toml` 已有但不用于 YAML)
- 现有 `add`/`search`/`delete`/`prune`/MCP 行为零改动
- 文件树记忆统一 `project=neuropils` 隔离
- 增量以 **title 为 join key**(与 import_wiki.py 一致);内容 hash = `MemoryApi::content_hash(content)`
- 边建立走 `EdgeApi::link`(幂等,UNIQUE 冲突取 max strength)
- 构建后必须 `codesign --force --sign -`(macOS 签名陷阱)
- 每次提交用 gitmoji 格式

---

### Task 1: frontmatter 解析器(neuropils.rs 基础)

**Files:**
- Create: `crates/mnemush/src/neuropils.rs`
- Modify: `crates/mnemush/src/lib.rs`(加 `pub mod neuropils;`)
- Modify: `crates/mnemush/src/schema.rs`(`Source` 加 `FileTree` 变体)
- Test: `crates/mnemush/src/neuropils.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  - `pub struct MemoryFile { pub path: PathBuf, pub title: String, pub category: Option<String>, pub tags: Vec<String>, pub links: Vec<String>, pub content: String, pub hash: String }`
  - `pub fn parse_file(path: &Path) -> crate::error::Result<Option<MemoryFile>>` — 非 `.md` 或无 frontmatter 返回 `None`;frontmatter 无 `title` 时用文件名(去 `.md`)兜底;hash = `MemoryApi::content_hash(content)`
  - `pub const PROJECT: &str = "neuropils";`
  - `pub const NEUROPIL_TAG_PREFIX: &str = "neuropil-path:";`

- [ ] **Step 1: 加 `Source::FileTree` 变体**

```rust
// schema.rs, Source enum 末尾(SearchResult 后):
    /// Imported from a markdown file tree (`mnemush import-tree`).
    FileTree,
// as_str():
            Source::FileTree => "file_tree",
```

- [ ] **Step 2: 注册模块 + 写失败的 frontmatter 解析测试**

```rust
// lib.rs 的 pub mod 列表(store 后):
pub mod neuropils;
```

```rust
// neuropils.rs 顶部 + tests:
use std::path::{Path, PathBuf};
use regex::Regex;
use crate::error::Result;
use crate::memory::MemoryApi;

pub const PROJECT: &str = "neuropils";
pub const NEUROPIL_TAG_PREFIX: &str = "neuropil-path:";

pub struct MemoryFile {
    pub path: PathBuf,
    pub title: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub content: String,
    pub hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_file(name: &str, text: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn parses_full_frontmatter() {
        let p = tmp_file("a.md",
            "---\ntitle: GitHub proxy setup\ncategory: lesson\ntags: [proxy, github]\nlinks: [\"b.md\", \"other-title\"]\n---\n# Body\nAccessing GitHub needs Clash 7890.\n");
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
}
```

- [ ] **Step 3: 实现 `parse_file`**

```rust
fn parse_frontmatter(text: &str) -> Option<(Vec<(String, String)>, usize)> {
    // returns (key-value pairs, index past closing `---`)
    if !text.starts_with("---\n") { return None; }
    let end = text[4..].find("\n---").map(|i| i + 4)?;
    let block = &text[4..end];
    let mut kv = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some((k, v)) = line.split_once(':') {
            kv.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some((kv, end + 4))
}

fn parse_list(v: &str) -> Vec<String> {
    let v = v.trim();
    if v.starts_with('[') && v.ends_with(']') {
        v[1..v.len() - 1].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    } else if v.is_empty() {
        vec![]
    } else {
        v.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }
}

pub fn parse_file(path: &Path) -> Result<Option<MemoryFile>> {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let (kv, body_start) = match parse_frontmatter(&text) {
        Some(x) => x,
        None => return Ok(None),
    };
    let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    let title = get("title").unwrap_or_else(|| {
        path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd crates/mnemush && cargo test --release neuropils`
Expected: 3 tests PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/neuropils.rs crates/mnemush/src/lib.rs crates/mnemush/src/schema.rs
git commit -m "✨ neuropils: frontmatter 解析 + Source::FileTree"
```

---

### Task 2: import-tree 命令(增量同步)

**Files:**
- Modify: `crates/mnemush/src/bin/cli.rs`(`Cmd` enum 加 `ImportTree`,match 分支)
- Modify: `crates/mnemush/src/neuropils.rs`(`import_tree` 函数)
- Test: `neuropils.rs` tests

**Interfaces:**
- Consumes: `MemoryFile`(Task 1)、`MemoryApi::add/update/list_in_project/content_hash`、`Source::FileTree`
- Produces:
  - `pub struct ImportStats { pub added: usize, pub updated: usize, pub skipped: usize, pub edges: usize }`
  - `pub fn import_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ImportStats>` — 遍历 `**/*.md`(排序保证确定性),按 title join 现有记忆:
    - hash 相同 → skipped
    - 存在且 hash 不同 → `api.update`(改 content/tags/category,保留 id/created_at)→ updated
    - 不存在 → `api.add(NewMemory::note(content, title) + category/tags/project=project/source=FileTree)` → added
    - 每条记忆打 tag `file-path:<相对路径>`

- [ ] **Step 1: 写失败的 import 测试(临时目录 → 记忆;重复 import 幂等;编辑后更新)**

```rust
// neuropils.rs tests, 追加:
    use crate::memory::{MemoryApi, SearchOpts};
    use crate::schema::{Category, MemoryType, NewMemory, Source};
    use crate::store::Store;
    use crate::config::Config;

    fn tmp_tree() -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-import-{}", std::process::id()));
        fs::create_dir_all(d.join("lesson/proxy")).unwrap();
        fs::create_dir_all(d.join("decision/rename")).unwrap();
        fs::write(d.join("lesson/proxy/clash.md"),
            "---\ntitle: Clash port 7890\ncategory: lesson\ntags: [proxy]\n---\nThe local Clash proxy listens on 127.0.0.1:7890.\n").unwrap();
        fs::write(d.join("decision/rename/rename.md"),
            "---\ntitle: Project rename\ncategory: decision\n---\nmneme was renamed to mnemush.\n").unwrap();
        d
    }

    #[test]
    fn import_creates_memories_and_is_idempotent() {
        let (store, cfg) = crate::memory::tests::store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree();
        let s1 = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s1.added, 2);
        let s2 = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s2.added, 0);
        assert_eq!(s2.skipped, 2);
        let hits = api.search("clash 7890", SearchOpts { limit: Some(5), ..Default::default() }).unwrap();
        assert!(hits.iter().any(|h| h.memory.title == "Clash port 7890"));
    }

    #[test]
    fn import_updates_edited_file_in_place() {
        let (store, cfg) = crate::memory::tests::store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree();
        import_tree(&api, &dir, PROJECT).unwrap();
        let p = dir.join("lesson/proxy/clash.md");
        fs::write(&p, "---\ntitle: Clash port 7890\ncategory: lesson\n---\nNow with a new detail: socks5 too.\n").unwrap();
        let s = import_tree(&api, &dir, PROJECT).unwrap();
        assert_eq!(s.updated, 1);
        assert_eq!(s.added, 0);
        let mems = api.list_in_project(100, Some(PROJECT.to_string())).unwrap();
        assert_eq!(mems.len(), 2, "edit updates, does not duplicate");
        let clash = mems.iter().find(|m| m.title == "Clash port 7890").unwrap();
        assert!(clash.content.contains("socks5"));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release neuropils::tests::import_`
Expected: FAIL(cannot find `import_tree`)

- [ ] **Step 3: 实现 `import_tree`**

```rust
pub struct ImportStats {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub edges: usize,
}

fn collect_md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
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
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

pub fn import_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ImportStats> {
    // Load existing memories of this project, keyed by title.
    let mut existing: std::collections::HashMap<String, crate::schema::Memory> =
        std::collections::HashMap::new();
    for m in api.list_in_project(100000, Some(project.to_string()))? {
        existing.insert(m.title.clone(), m);
    }
    let mut stats = ImportStats { added: 0, updated: 0, skipped: 0, edges: 0 };
    let files = collect_md_files(dir)?;
    for path in files {
        let mf = match parse_file(&path)? {
            Some(m) => m,
            None => continue,
        };
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().to_string();
        match existing.get(&mf.title) {
            Some(old) if old.content_hash == mf.hash => { stats.skipped += 1; }
            Some(old) => {
                let mut m = old.clone();
                m.content = mf.content;
                // update_memory_tx 不重算 hash —— 必须手动更新, 否则下次 import 误判为未变
                m.content_hash = MemoryApi::content_hash(&m.content);
                m.category = mf.category.as_deref().and_then(crate::schema::Category::parse)
                    .unwrap_or(crate::schema::Category::Note);
                m.tags = mf.tags.clone();
                if !m.tags.iter().any(|t| t.starts_with(NEUROPIL_TAG_PREFIX)) {
                    m.tags.push(format!("{NEUROPIL_TAG_PREFIX}{rel}"));
                }
                api.update(&m)?;
                stats.updated += 1;
            }
            None => {
                let mut nm = NewMemory::note(mf.content.clone(), mf.title.clone());
                nm.category = mf.category.as_deref().and_then(crate::schema::Category::parse)
                    .unwrap_or(crate::schema::Category::Note);
                nm.tags = {
                    let mut t = mf.tags.clone();
                    t.push(format!("{NEUROPIL_TAG_PREFIX}{rel}"));
                    t
                };
                nm.project = Some(project.to_string());
                nm.source = Source::FileTree;
                api.add(nm)?;
                stats.added += 1;
            }
        }
    }
    Ok(stats)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release neuropils::tests::import_`
Expected: 2 tests PASS

- [ ] **Step 5: CLI 挂 `import-tree` 子命令**

```rust
// cli.rs, Cmd enum(Search 前插入):
    /// Import a markdown file tree (a neuropil) as memories (file = source of truth).
    ImportTree {
        /// Directory to scan (default: ~/.mnemush/neuropils).
        dir: Option<String>,
        /// Project id for this neuropil (default: neuropils).
        #[arg(long)]
        project: Option<String>,
    },
// match 分支:
        Cmd::ImportTree { dir } => {
            let d = dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(
                    std::env::var_os("MNEMUSH_MEMORIES_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            std::path::PathBuf::from(dirs_home()).join(".mnemush").join("neuropils")
                        }),
                ));
            std::fs::create_dir_all(&d)?;
            let api = MemoryApi::new(&store, &config);
            let proj = project.clone().unwrap_or_else(|| mnemush::neuropils::PROJECT.to_string());
            let s = mnemush::neuropils::import_tree(&api, &d, &proj)?;
            println!("imported: +{} added, {} updated, {} skipped",
                     s.added, s.updated, s.skipped);
        }
```

- [ ] **Step 6: 编译 + 全量测试 + 提交**

Run: `cargo test --release`
Expected: 全部 PASS(原 112 + 新增)

```bash
git add crates/mnemush/src/neuropils.rs crates/mnemush/src/bin/cli.rs
git commit -m "✨ import-tree: 文件树增量同步(按 title join, 编辑即更新)"
```

---

### Task 3: wikilink → 非树状边(海马层)

**Files:**
- Modify: `crates/mnemush/src/neuropils.rs`
- Test: `neuropils.rs` tests

**Interfaces:**
- Consumes: `MemoryFile.links`、`EdgeApi::link`
- Produces: 无新公开接口;`import_tree` 内建边:显式 `links` → related(或 `supports:` 前缀 → supports);正文 `[[target]]` 也解析进 links

- [ ] **Step 1: 写失败的边测试**

```rust
// neuropils.rs tests 追加:
    fn tmp_tree_linked() -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-link-{}", std::process::id()));
        fs::create_dir_all(d.join("a")).unwrap();
        fs::create_dir_all(d.join("b")).unwrap();
        fs::write(d.join("a/x.md"),
            "---\ntitle: Alpha\ntags: [t1]\nlinks: [\"b/y.md\"]\n---\nAlpha content about thing one.\n").unwrap();
        fs::write(d.join("b/y.md"),
            "---\ntitle: Beta\ntags: [t2]\n---\nBeta content mentions [[Alpha]] inline.\n").unwrap();
        d
    }

    #[test]
    fn wikilinks_create_edges_between_memories() {
        let (store, cfg) = crate::memory::tests::store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree_linked();
        import_tree(&api, &dir, PROJECT).unwrap();
        let alpha = api.list_in_project(100, Some(PROJECT.to_string())).unwrap()
            .into_iter().find(|m| m.title == "Alpha").unwrap();
        let beta = api.list_in_project(100, Some(PROJECT.to_string())).unwrap()
            .into_iter().find(|m| m.title == "Beta").unwrap();
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let edges = eapi.neighbors_of(&alpha.id).unwrap();
        assert!(edges.iter().any(|(m, _)| m.id == beta.id), "Alpha should be linked to Beta");
        let edges2 = eapi.neighbors_of(&beta.id).unwrap();
        assert!(edges2.iter().any(|(m, _)| m.id == alpha.id), "Beta should be linked to Alpha (inline wikilink)");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release neuropils::tests::wikilinks`
Expected: FAIL(`neighbors_of` 不存在或边未建)

- [ ] **Step 3: 实现链接解析 + 建边**

```rust
// 正文内 [[target]] 提取(与 frontmatter links 合并):
fn inline_wikilinks(content: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").unwrap();
    re.captures_iter(content).map(|c| c[1].trim().to_string()).collect()
}

fn resolve_target<'a>(files: &'a [MemoryFile], existing: &'a std::collections::HashMap<String, crate::schema::Memory>, target: &str) -> Option<&'a str> {
    // 1. 相对路径 → 文件 title; 2. 直接 title 匹配
    if let Some(f) = files.iter().find(|f| {
        f.path.to_string_lossy().ends_with(target.trim_start_matches("./"))
    }) {
        return existing.get(&f.title).map(|m| m.id.as_str());
    }
    existing.get(target.trim()).map(|m| m.id.as_str())
}
```

```rust
// import_tree 内,在遍历完文件后统一建边(需要所有文件 parse 结果):
        // (改造 import_tree: 先 collect 所有 (MemoryFile, Option<memory_id>) 对,
        //  再建边 — 保证前向链接也能解析)
        let edge_api = crate::edge::EdgeApi::new(api.store, api.config);
        for (mf, id) in &parsed {
            let mut targets = mf.links.clone();
            targets.extend(inline_wikilinks(&mf.content));
            for t in targets {
                let tid = match resolve_target(&parsed, &existing, &t) {
                    Some(x) => x,
                    None => continue, // 目标缺失,跳过
                };
                let etype = if t.starts_with("supports:") { crate::schema::EdgeType::Supports }
                            else { crate::schema::EdgeType::Related };
                let _ = edge_api.link(id, tid, etype, 0.6, Some("neuropil:wikilink"), None)?;
                stats.edges += 1;
            }
        }
```

> 注意:上面引用 `parsed: Vec<(MemoryFile, String /*memory id*/)>` 和 `eapi.neighbors_of` — Task 3 需把 Task 2 的 `import_tree` 改为先全量 parse(带 id 解析),再统一建边(当前 Task 2 版本是边建边处理;若 Task 2 已提交为增量流,此处把"收集 parsed"提前到循环前即可,不影响 skipped/updated 逻辑)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release neuropils::tests::wikilinks`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/mnemush/src/neuropils.rs
git commit -m "✨ neuropils: wikilink → memory_edge(海马层跨簇连接)"
```

---

### Task 4: export-tree 命令

**Files:**
- Modify: `crates/mnemush/src/bin/cli.rs`
- Modify: `crates/mnemush/src/neuropils.rs`
- Test: `neuropils.rs` tests

**Interfaces:**
- Consumes: `MemoryApi::list_in_project`
- Produces: `pub fn export_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ExportStats>`(`ExportStats { pub written: usize }`)— 按 `<category>/<topic>/<title>.md` 写文件(frontmatter: title/category/tags/links 从边反向?links 不导出,保持简单)

- [ ] **Step 1: 写失败的往返测试**

```rust
// neuropils.rs tests 追加:
    #[test]
    fn export_then_import_roundtrip() {
        let (store, cfg) = crate::memory::tests::store();
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
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release neuropils::tests::export_then_import`
Expected: FAIL(`export_tree` 不存在)

- [ ] **Step 3: 实现 `export_tree`**

```rust
pub struct ExportStats { pub written: usize }

pub fn export_tree(api: &MemoryApi, dir: &Path, project: &str) -> Result<ExportStats> {
    let mems = api.list_in_project(100000, Some(project.to_string()))?;
    let mut stats = ExportStats { written: 0 };
    for m in mems {
        if m.deleted_at.is_some() { continue; }
        let cat_dir = dir.join(m.category.as_str());
        fs::create_dir_all(&cat_dir)?;
        let fname = format!("{}.md", sanitize_filename(&m.title));
        let tags = if m.tags.is_empty() { String::new() } else {
            format!("tags: [{}]\n", m.tags.iter().map(|t| t.clone()).collect::<Vec<_>>().join(", "))
        };
        let fm = format!(
            "---\ntitle: {}\ncategory: {}\n{links}---\n\n{}\n",
            m.title, m.category.as_str(), links = tags, m.content
        );
        fs::write(cat_dir.join(&fname), fm)?;
        stats.written += 1;
    }
    Ok(stats)
}

fn sanitize_filename(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c.is_whitespace() || c == '-' || c == '_' { c } else { '-' }).collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release neuropils::tests::export_then_import`
Expected: PASS

- [ ] **Step 5: CLI 挂 `export-tree` + 提交**

```rust
// cli.rs, Cmd enum(ImportTree 后):
    /// Export memories of a project to a markdown file tree.
    ExportTree {
        /// Destination directory (default: ~/.mnemush/neuropils).
        dir: Option<String>,
        /// Project id of the neuropil to export (default: neuropils).
        #[arg(long)]
        project: Option<String>,
    },
// match:
        Cmd::ExportTree { dir } => {
            let d = dir.map(std::path::PathBuf::from).unwrap_or_else(|| {
                std::path::PathBuf::from(dirs_home()).join(".mnemush").join("neuropils")
            });
            let api = MemoryApi::new(&store, &config);
            let proj = project.clone().unwrap_or_else(|| mnemush::neuropils::PROJECT.to_string());
            let s = mnemush::neuropils::export_tree(&api, &d, &proj)?;
            println!("exported {} memories to {}", s.written, d.display());
        }
```

```bash
git add crates/mnemush/src/neuropils.rs crates/mnemush/src/bin/cli.rs
git commit -m "✨ export-tree: 记忆落盘为 markdown 文件树"
```

---

### Task 5: 同目录共现自动边 + 收尾

**Files:**
- Modify: `crates/mnemush/src/neuropils.rs`
- Test: `neuropils.rs` tests

**Interfaces:**
- Consumes: 前序任务的 `import_tree` 内部结构
- Produces: 无新公开接口;import 时同目录文件两两建 weak related 边(`strength 0.3`,provenance `neuropil:copath`)

- [ ] **Step 1: 写失败的共现边测试**

```rust
    #[test]
    fn same_directory_files_get_weak_edges() {
        let (store, cfg) = crate::memory::tests::store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = std::env::temp_dir().join(format!("neuropil-copath-{}", std::process::id()));
        fs::create_dir_all(dir.join("topic1")).unwrap();
        fs::write(dir.join("topic1/m1.md"), "---\ntitle: M1\n---\nContent of m1.\n").unwrap();
        fs::write(dir.join("topic1/m2.md"), "---\ntitle: M2\n---\nContent of m2.\n").unwrap();
        fs::write(dir.join("topic1/m3.md"), "---\ntitle: M3\n---\nContent of m3.\n").unwrap();
        fs::write(dir.join("other.md"), "---\ntitle: Other\n---\nUnrelated content elsewhere.\n").unwrap();
        import_tree(&api, &dir, PROJECT).unwrap();
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let m1 = api.list_in_project(100, Some(PROJECT.to_string())).unwrap()
            .into_iter().find(|m| m.title == "M1").unwrap();
        let others: Vec<String> = api.list_in_project(100, Some(PROJECT.to_string())).unwrap()
            .into_iter().filter(|m| m.title.starts_with('M')).map(|m| m.id).collect();
        let neighbors: Vec<String> = eapi.neighbors_of(&m1.id).unwrap().into_iter().map(|(m, _)| m.id).collect();
        for oid in &others {
            if oid != &m1.id {
                assert!(neighbors.contains(oid), "M1 linked to sibling in same dir");
            }
        }
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release neuropils::tests::same_directory`
Expected: FAIL(M1 无兄弟边)

- [ ] **Step 3: 实现共现边(import_tree 末尾)**

```rust
// import_tree 内,在建完 wikilink 边后:
        // 同目录共现:两两建 weak related 边(provenance=neuropil:copath)。
        let mut by_dir: std::collections::HashMap<String, Vec<&(MemoryFile, String)>> =
            std::collections::HashMap::new();
        for item in &parsed {
            let d = item.0.path.parent().unwrap_or(Path::new(""))
                .to_string_lossy().to_string();
            by_dir.entry(d).or_default().push(item);
        }
        for (_d, group) in by_dir {
            if group.len() < 2 { continue; }
            for i in 0..group.len() {
                for j in (i + 1)..group.len() {
                    let _ = edge_api.link(&group[i].1, &group[j].1,
                        crate::schema::EdgeType::Related, 0.3,
                        Some("neuropil:copath"), None)?;
                    stats.edges += 1;
                }
            }
        }
```

- [ ] **Step 4: 跑测试确认通过 + 全量回归**

Run: `cargo test --release`
Expected: 全部 PASS

- [ ] **Step 5: 安装 + 重签 + 端到端验证 + 提交**

```bash
cp crates/mnemush/target/release/mnemush crates/mnemush/target/release/mnemush-mcp ~/.cargo/bin/
codesign --force --sign - ~/.cargo/bin/mnemush ~/.cargo/bin/mnemush-mcp
```

```bash
# 端到端:建示例树 → import → search → export
mkdir -p /tmp/demo-mem/lesson/proxy
cat > /tmp/demo-mem/lesson/proxy/clash.md <<'EOF'
---
title: Clash 7890
category: lesson
links: ["../rename/rename.md"]
---
Clash listens on 127.0.0.1:7890; curl needs HTTPS_PROXY.
EOF
mkdir -p /tmp/demo-mem/decision/rename
cat > /tmp/demo-mem/decision/rename/rename.md <<'EOF'
---
title: Rename note
category: decision
---
Project renamed mneme to mnemush.
EOF
mnemush import-tree /tmp/demo-mem
mnemush search "clash proxy" --project neuropils
mnemush search "rename" --project neuropils
```

Expected: import 2 added;两条搜索各命中;重复 import 输出 0 added 2 skipped。

```bash
# CHANGELOG 追加 v1.1 条目,再提交
git add crates/mnemush/src/neuropils.rs CHANGELOG.md
git commit -m "✨ neuropils: 同目录共现自动边 + v1.1 changelog"
```

---

## Self-Review 记录

- **Spec 覆盖**:import-tree(Task 2)✓ / export-tree(Task 4)✓ / wikilink 显式边(Task 3)✓ / 自动推断:内容相似度(auto-link 由 api.add 自动触发,零代码)✓ + 向量近邻(add 走现有嵌入流程)✓ + 同目录共现(Task 5)✓ / 双轨共存(未改 add)✓ / watch 与 add 改造=非目标 ✓
- **类型一致**:`MemoryFile` 字段在 Task 1 定义、Task 2-5 消费;`ImportStats`/`ExportStats` 名字前后一致;`import_tree(api, dir, project)` 签名 Task 2-5 统一
- **注意点**:Task 3 的"先全量 parse 再建边"需对 Task 2 的循环结构做一次前置重构(把 parsed 收集提前),Task 3 Step 3 已注明

---

### Task 6: external-wiki 收编验证(wiki 链接格式兼容)

**Files:**
- Modify: `crates/mnemush/src/neuropils.rs`
- Modify: `scripts/import_wiki.py`(头部注释标注可被 import-tree 取代)
- Test: `neuropils.rs` tests

**Interfaces:**
- Consumes: `import_tree`(Task 2-5)
- Produces: 链接解析同时支持 `[[target]]` 与 wiki 风格 `[label](path/ID)`;验证 external-wiki 目录可作为 neuropil 导入(不迁移现有 5505 条数据)

- [ ] **Step 1: 写失败的 wiki 链接格式测试**

```rust
// neuropils.rs tests 追加:
    fn tmp_tree_wiki_style() -> PathBuf {
        let d = std::env::temp_dir().join(format!("neuropil-wiki-{}", std::process::id()));
        fs::create_dir_all(d.join("concepts")).unwrap();
        fs::create_dir_all(d.join("papers")).unwrap();
        fs::write(d.join("concepts/sleep.md"),
            "---\ntitle: Sleep\n---\nA state studied in [Drosophila sleep paper](papers/AAA111).\n").unwrap();
        fs::write(d.join("papers/AAA111.md"),
            "---\ntitle: Drosophila sleep paper\n---\nStudy on sleep in fruit flies.\n").unwrap();
        d
    }

    #[test]
    fn wiki_style_links_create_edges() {
        let (store, cfg) = crate::memory::tests::store();
        let api = MemoryApi::new(&store, &cfg);
        let dir = tmp_tree_wiki_style();
        import_tree(&api, &dir, "external-wiki-sample").unwrap();
        let eapi = crate::edge::EdgeApi::new(&store, &cfg);
        let sleep = api.list_in_project(100, Some("external-wiki-sample".to_string())).unwrap()
            .into_iter().find(|m| m.title == "Sleep").unwrap();
        let paper = api.list_in_project(100, Some("external-wiki-sample".to_string())).unwrap()
            .into_iter().find(|m| m.title == "Drosophila sleep paper").unwrap();
        let neigh: Vec<String> = eapi.neighbors_of(&sleep.id).unwrap().into_iter().map(|(m, _)| m.id).collect();
        assert!(neigh.contains(&paper.id), "wiki-style [label](papers/X) link becomes an edge");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --release neuropils::tests::wiki_style`
Expected: FAIL(边未建,`[label](...)` 未被识别)

- [ ] **Step 3: 实现 wiki 风格链接解析(并入 inline_wikilinks / resolve_target)**

```rust
// inline_wikilinks 增强:同时匹配 [[target]] 与 [label](path/ID)
fn inline_wikilinks(content: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]|\[[^\]]*\]\(([^)]+)\)").unwrap();
    re.captures_iter(content)
        .map(|c| c.get(1).map(|m| m.as_str().trim().to_string())
            .or_else(|| c.get(2).map(|m| m.as_str().trim().to_string()))
            .unwrap_or_default())
        .filter(|s| !s.is_empty())
        .collect()
}
```

```rust
// resolve_target 增强:wiki 风格 target 形如 "papers/AAA111" → 匹配文件名(不含扩展名)
// 现有逻辑已按路径后缀匹配(ends_with),天然兼容 "papers/AAA111" 与 "AAA111.md"。
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --release neuropils::tests::wiki_style`
Expected: PASS

- [ ] **Step 5: 标注 import_wiki.py 收编路径 + 提交**

```bash
# scripts/import_wiki.py 头部注释追加:
# NOTE: 本脚本可被 `mnemush import-tree <wiki-dir> --project external-wiki` 取代
# (neuropils 通用导入器, 链接格式兼容 wiki 风格 [label](papers/X))。存量数据不迁移。
```

```bash
git add crates/mnemush/src/neuropils.rs scripts/import_wiki.py
git commit -m "✨ neuropils: 兼容 wiki 链接格式, external-wiki 可收编为 neuropil"
```
