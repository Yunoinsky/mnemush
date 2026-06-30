//! `mneme` CLI — terminal interface for the memory layer.
//!
//! Subcommands:
//! - search    : FTS5 search
//! - add       : add a memory
//! - get       : get a memory by id
//! - list      : list recent memories
//! - delete    : soft-delete a memory
//! - stats     : statistics
//! - config    : config inspection
//! - init      : bootstrap ~/.mneme with templates

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mneme::config::Config;
use mneme::identity::Identity;
use mneme::memory::MemoryApi;
use mneme::schema::{Category, MemoryType, NewMemory, SearchOpts, Source};
use mneme::store::Store;
use mneme::{expand_tilde, init_tracing};

#[derive(Parser)]
#[command(
    name = "mneme",
    version,
    about = "Brain-inspired memory for AI coding agents"
)]
struct Cli {
    /// Path to a custom config file (default: ~/.mneme/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Path to a custom database file (overrides config).
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Search memories.
    Search {
        query: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long, short = 'l')]
        limit: Option<usize>,
        #[arg(long)]
        project: Option<String>,
    },
    /// Add a new memory.
    Add {
        title: String,
        content: String,
        #[arg(long, short = 'c', default_value = "note")]
        category: String,
        #[arg(long, short = 'i', default_value_t = 0.5)]
        importance: f32,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Get a memory by id.
    Get { id: String },
    /// List recent memories.
    List {
        #[arg(long, short = 'l', default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        category: Option<String>,
    },
    /// Soft-delete a memory.
    Delete { id: String },
    /// Show stats.
    Stats,
    /// Show identity (USER/PERSONA/CONSTITUTION).
    Identity,
    /// Config inspection.
    Config,
    /// Initialize ~/.mneme with template files.
    Init,
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let config = if let Some(p) = &cli.config {
        Config::load_from(p)?
    } else {
        Config::load()?
    };

    let db_path = cli
        .db
        .clone()
        .unwrap_or_else(|| expand_tilde(&config.storage.db_path));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let store = Store::open(&db_path)?;

    match cli.cmd {
        Cmd::Search {
            query,
            category,
            limit,
            project,
        } => {
            let opts = SearchOpts {
                category: category.as_deref().and_then(Category::parse),
                project,
                limit,
                ..Default::default()
            };
            let api = MemoryApi::new(&store, &config);
            let hits = api.search(&query, opts)?;
            if hits.is_empty() {
                println!("(no matches)");
                return Ok(());
            }
            for h in hits {
                println!(
                    "[{:.2}] {} ({})  #{}",
                    h.score,
                    h.memory.title,
                    h.memory.category.as_str(),
                    &h.memory.id[..8]
                );
                if !h.memory.content.is_empty() {
                    println!("       {}", truncate(&h.memory.content, 80));
                }
            }
        }
        Cmd::Add {
            title,
            content,
            category,
            importance,
            project,
            tags,
        } => {
            let cat = Category::parse(&category).unwrap_or(Category::Note);
            let mut m = NewMemory::note(content, title);
            m.category = cat;
            m.memory_type = MemoryType::Semantic;
            m.importance = importance;
            m.project = project;
            m.source = Source::Manual;
            if let Some(t) = tags {
                m.tags = t.split(',').map(|s| s.trim().to_string()).collect();
            }
            let api = MemoryApi::new(&store, &config);
            let r = api.add(m)?;
            println!("added: {}", r.id);
            if !r.conflicts.is_empty() {
                println!("⚠ {} conflict(s):", r.conflicts.len());
                for c in r.conflicts {
                    println!("  - {} ({})", c.title, c.category.as_str());
                }
            }
        }
        Cmd::Get { id } => {
            let api = MemoryApi::new(&store, &config);
            let m = api.get(&id)?;
            match m {
                Some(m) => print_memory(&m),
                None => println!("(not found)"),
            }
        }
        Cmd::List { limit, category } => {
            let api = MemoryApi::new(&store, &config);
            let mut mems = api.list(limit)?;
            if let Some(c) = category.as_deref().and_then(Category::parse) {
                mems.retain(|m| m.category == c);
            }
            for m in mems {
                println!(
                    "[{}] {} ({})  #{}",
                    m.memory_type.as_str(),
                    m.title,
                    m.category.as_str(),
                    &m.id[..8]
                );
            }
        }
        Cmd::Delete { id } => {
            let api = MemoryApi::new(&store, &config);
            api.soft_delete(&id)?;
            println!("soft-deleted: {}", id);
        }
        Cmd::Stats => {
            let count: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let edges: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory_edge WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let by_type = count_by(&store, "memory_type")?;
            let by_cat = count_by(&store, "category")?;
            println!("memories:    {}", count);
            println!("edges:       {}", edges);
            println!("by type:");
            for (k, v) in by_type {
                println!("  {}: {}", k, v);
            }
            println!("by category:");
            for (k, v) in by_cat {
                println!("  {}: {}", k, v);
            }
        }
        Cmd::Identity => {
            let id = Identity::load().unwrap_or_default();
            if id.is_empty() {
                println!("(no identity files in ~/.mneme/identity/)");
                println!("run `mneme init` to bootstrap");
            } else {
                println!("{}", id.render_prompt_block());
            }
        }
        Cmd::Config => {
            println!("{:#?}", config);
        }
        Cmd::Init => {
            init_dotfiles()?;
        }
    }
    Ok(())
}

fn print_memory(m: &mneme::schema::Memory) {
    println!("id:          {}", m.id);
    println!("type:        {}", m.memory_type.as_str());
    println!("category:    {}", m.category.as_str());
    println!("title:       {}", m.title);
    println!("content:     {}", m.content);
    if let Some(ctx) = &m.context {
        println!("context:     {}", ctx);
    }
    if let Some(tk) = &m.topic_key {
        println!("topic_key:   {}", tk);
    }
    println!("importance:  {:.2}", m.importance);
    println!("confidence:  {:.2}", m.confidence);
    println!("access:      {}", m.access_count);
    println!("created:     {}", m.created_at);
    println!("accessed:    {}", m.last_accessed_at);
}

fn count_by(store: &Store, col: &str) -> anyhow::Result<Vec<(String, i64)>> {
    let sql = format!(
        "SELECT {}, COUNT(*) FROM memory WHERE deleted_at IS NULL GROUP BY {}",
        col, col
    );
    let mut stmt = store.conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let k: String = r.get(0)?;
        let v: i64 = r.get(1)?;
        Ok((k, v))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn init_dotfiles() -> anyhow::Result<()> {
    let home = std::env::var("HOME")?;
    let data_dir = std::path::PathBuf::from(&home).join(".mneme");
    let id_dir = data_dir.join("identity");
    std::fs::create_dir_all(&id_dir)?;

    // Copy config example
    let config_dst = data_dir.join("config.toml");
    if !config_dst.exists() {
        std::fs::write(
            &config_dst,
            include_str!("../../../../docs/config.example.toml"),
        )?;
        println!("wrote {}", config_dst.display());
    } else {
        println!("(skipped, exists) {}", config_dst.display());
    }

    // Copy identity templates
    for (name, body) in [
        ("USER.md", include_str!("../../../../docs/identity/USER.md")),
        (
            "PERSONA.md",
            include_str!("../../../../docs/identity/PERSONA.md"),
        ),
        (
            "CONSTITUTION.md",
            include_str!("../../../../docs/identity/CONSTITUTION.md"),
        ),
    ] {
        let dst = id_dir.join(name);
        if !dst.exists() {
            std::fs::write(&dst, body)?;
            println!("wrote {}", dst.display());
        } else {
            println!("(skipped, exists) {}", dst.display());
        }
    }
    println!("✓ initialized {}", data_dir.display());
    Ok(())
}
