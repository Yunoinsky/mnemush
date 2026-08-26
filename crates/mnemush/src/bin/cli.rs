//! `mnemush` CLI — terminal interface for the memory layer.
//!
//! Subcommands:
//! - search    : FTS5 search
//! - add       : add a memory
//! - get       : get a memory by id
//! - list      : list recent memories
//! - delete    : soft-delete a memory
//! - stats     : statistics
//! - config    : config inspection
//! - init      : bootstrap ~/.mnemush with templates

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mnemush::config::Config;
use mnemush::forget::{self, PruneReason};
use mnemush::identity::Identity;
use mnemush::memory::MemoryApi;
use mnemush::schema::{Category, MemoryType, NewMemory, SearchOpts, Source};
use mnemush::store::Store;
use mnemush::{expand_tilde, init_tracing};

#[derive(Parser)]
#[command(
    name = "mnemush",
    version,
    about = "Brain-inspired memory for AI coding agents"
)]
struct Cli {
    /// Path to a custom config file (default: ~/.mnemush/config.toml).
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
    /// Import a markdown file tree (a neuropil) as memories (file = source of truth).
    ImportTree {
        /// Directory to scan (default: ~/.mnemush/neuropils).
        dir: Option<String>,
        /// Project id for this neuropil (default: neuropils).
        #[arg(long)]
        project: Option<String>,
    },
    /// Export memories of a project to a markdown file tree (a neuropil).
    ExportTree {
        /// Destination directory (default: ~/.mnemush/neuropils).
        dir: Option<String>,
        /// Project id of the neuropil to export (default: neuropils).
        #[arg(long)]
        project: Option<String>,
    },
    /// Consolidate memories: LLM-driven integration + active forgetting.
    Consolidate {
        /// Project filter (default: all projects).
        #[arg(long)]
        project: Option<String>,
        /// Print parsed actions without executing or advancing position.
        #[arg(long)]
        dry_run: bool,
        /// Print the raw LLM suggestion JSON without executing.
        #[arg(long)]
        suggest: bool,
        /// Only consider memories created after this unix timestamp
        /// (default: last consolidated position).
        #[arg(long)]
        since: Option<i64>,
    },
    /// Dream: full-corpus consolidation with stronger forgetting
    /// (sleep-replay analogy). Same engine as consolidate.
    Dream {
        /// Project filter (default: all projects).
        #[arg(long)]
        project: Option<String>,
        /// Print parsed actions without executing.
        #[arg(long)]
        dry_run: bool,
        /// Print the raw LLM suggestion JSON without executing.
        #[arg(long)]
        suggest: bool,
    },
    /// Run the dream scheduler daemon (v1.6.1). Long-running; wakes
    /// at `[dream] scheduled_time` (default 02:00 local), checks the
    /// daily token budget, then spawns `mnemush dream`. Single-instance
    /// via `<data_dir>/daemon.pid`. Configure `[dream]` in
    /// `~/.mnemush/config.toml` or via env (`MNEMUSH_DREAM_*`).
    Daemon {
        /// Dry-run: print next wake time + gate status, then exit.
        #[arg(long)]
        dry_run: bool,
    },
    /// v1.6.1: scan the running binary + PATH siblings for the
    /// pre-edef25b FK-cascade pattern. Exits 0 if all binaries are
    /// safe, 1 if any are stale. Use this to verify auto-sync is
    /// safe to re-enable after a `cargo install`.
    WebdavSafetyCheck,
    /// List top concepts (memory index) for agent priming.
    Concepts {
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// text (default) or json.
        #[arg(long)]
        format: Option<String>,
    },
    /// Search memories.
    Search {
        query: String,
        #[arg(long)]
        category: Option<String>,
        #[arg(long, short = 'l')]
        limit: Option<usize>,
        #[arg(long)]
        project: Option<String>,
        /// Bypass MNEMUSH_PROJECT isolation and search every project.
        #[arg(long)]
        all_projects: bool,
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
        /// Bypass MNEMUSH_PROJECT isolation and list every project.
        #[arg(long)]
        all_projects: bool,
        /// v1.6.2: filter to memories whose `origin_device` matches this
        /// device id (full UUID) or name (substring match). Combine
        /// with `all_projects` for unscoped listing.
        #[arg(long)]
        origin: Option<String>,
    },
    /// Soft-delete a memory.
    Delete { id: String },
    /// Prune (soft-delete) low-confidence / stale memories.
    /// Default mode is dry-run: lists candidates without touching the DB.
    Prune {
        /// Actually apply (default is dry-run).
        #[arg(long)]
        apply: bool,
        /// Cap the number of memories processed.
        #[arg(long, short = 'l')]
        limit: Option<usize>,
        /// Step 2: hard-delete soft-deleted memories that are also isolated
        /// (zero inbound edges, low importance, stale).
        #[arg(long)]
        isolate: bool,
        /// Days after soft-delete before hard-delete is allowed (--isolate).
        #[arg(long, default_value_t = 7)]
        grace_days: i64,
        /// Max importance for --isolate candidates (default 0.5).
        #[arg(long, default_value_t = 0.5)]
        max_importance: f32,
        /// Min days since last access for --isolate candidates.
        #[arg(long, default_value_t = 30)]
        min_days_no_access: i64,
    },
    /// v1.6.2: backfill the `origin_device` column for memories that
    /// predate the v4→v5 migration. Stamps NULL rows with a chosen
    /// device id. Use once on the machine that originally created
    /// the data, before any cross-device pull.
    ///   `mnemush memory reorigin --local`   — stamp with this device's id.
    ///   `mnemush memory reorigin --device ID` — stamp with an explicit id.
    MemoryReorigin {
        #[arg(long)]
        local: bool,
        #[arg(long, conflicts_with = "local")]
        device: Option<String>,
    },
    /// Show stats.
    Stats,
    /// One-line summary of memory system state: counts, pending
    /// identity proposals, reflect candidates. Designed for the LLM
    /// (and the user) to see at a glance without running separate
    /// commands.
    Status,
    /// Apply Ebbinghaus decay to all active edges (same formula as
    /// memory confidence decay). Useful as a manual or scheduled
    /// graph-cleanup pass. Idempotent.
    EdgeDecay,
    /// Process the needs_review queue: clear flags on items older
    /// than the grace period (default 1 day). For failure-category
    /// items, also downgrade importance by 0.1 per pass.
    ProcessNeedsReview {
        /// Grace period before a needs_review item is processed.
        #[arg(long, default_value = "1")]
        grace_days: i64,
    },
    /// Show or manage identity (USER/PERSONA/CONSTITUTION).
    /// Use `mnemush identity show` to print current files, or the
    /// subcommands below to manage LLM-proposed updates.
    Identity {
        #[command(subcommand)]
        action: IdentityCmd,
    },
    /// Config inspection.
    Config,
    /// Initialize ~/.mnemush with template files.
    Init,
    /// Surface recent, under-connected memories for LLM reflection.
    /// Prints each candidate's id, title, category, importance, and edge
    /// count. The LLM (or a human) reads these and decides which conceptual
    /// links the auto-link layer missed.
    Reflect {
        /// Only consider memories created in the last N days.
        #[arg(long, default_value_t = 7)]
        since_days: i64,
        /// Max candidates to surface.
        #[arg(long, short = 'l', default_value_t = 20)]
        limit: usize,
    },
    /// Self-evaluation observability. Read-only inspection of the
    /// per-session NDJSON log written by agent plugins. Stats
    /// summarizes call counts, per-tool breakdown, latency
    /// percentiles, and error rate. Dump emits raw NDJSON to
    /// stdout for offline analysis. The goal is to ground claims
    /// about "mnemush works well" in real usage data, not vibes.
    Eval {
        #[command(subcommand)]
        action: EvalCmd,
    },
    /// Graph analytics over the memory network: PageRank hub
    /// detection, community discovery (label propagation), and
    /// DOT / D3-JSON export for visualization.
    Graph {
        #[command(subcommand)]
        action: GraphCmd,
    },
    /// Cross-machine sync (v1.0) — Git as the transport, mnemush as
    /// the codec. Export the current DB state to a git repo, import
    /// a repo's state back. Run `git push`/`pull` on the repo
    /// yourself between machines.
    Sync {
        #[command(subcommand)]
        action: SyncCmd,
    },
    /// Rebuild the FTS5 search index from the memory table. Fixes
    /// rowid misalignment caused by historical soft/hard deletes
    /// that left orphaned FTS rows (search returned wrong content).
    /// Safe to run anytime; idempotent.
    Reindex,
    /// Backup and restore the entire `~/.mnemush/` data directory to a
    /// gzipped tar archive. Round-trip: a fresh `mnemush restore` into
    /// an empty target dir restores every memory, identity file,
    /// pending proposal, and self-eval log entry.
    Backup {
        /// Output path (default: ~/mnemush-backup-<UTC>.tar.gz).
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Include the eval/ NDJSON log (regenerable, default off).
        #[arg(long)]
        include_eval: bool,
    },
    /// v1.6.2: show this device's identity. With a positional `NAME`,
    /// rename this device instead. e.g. `mnemush device` (show),
    /// `mnemush device macbook-air-5` (rename).
    Device {
        /// Optional new name. If omitted, prints current identity.
        name: Option<String>,
    },
    /// Embed all (or selected) memories with the configured model.
    /// Requires `[embeddings] enabled = true`. First run downloads
    /// the model (~25 MB) to `~/.mnemush/models/`.
    Embed {
        /// Only embed memories matching this substring in the title
        /// (case-insensitive). Default: every active memory.
        #[arg(long)]
        title_contains: Option<String>,
        /// Skip if the memory already has an embedding for the
        /// configured model (default: re-embed all). Use --force to
        /// overwrite (e.g. after upgrading to a new model id).
        #[arg(long)]
        force: bool,
    },
    /// Restore a backup archive. Refuses to overwrite a target whose
    /// schema_version is newer than the backup (downgrade protection);
    /// pass `--force` to override. Prompts for confirmation by default.
    Restore {
        /// Backup archive path.
        #[arg(long, short = 'i')]
        input: String,
        /// Target directory (default: ~/.mnemush).
        #[arg(long)]
        target: Option<String>,
        /// Skip confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Allow downgrade (overwrite a newer DB).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum EvalCmd {
    /// Summarize the eval log: total calls, per-tool breakdown,
    /// p50/p95 latency, error rate. Output is human-readable
    /// for `tail -f` style watching or piping into `less`.
    Stats {
        /// Only count events with ts newer than (now - since). Accepts
        /// the same forms as `mnemush reflect --since-days` (e.g. "1d",
        /// "12h"). Default: 7d.
        #[arg(long, default_value = "7d")]
        since: String,
    },
    /// Emit the raw NDJSON log (filtered by --since) to stdout,
    /// one entry per line. For piping into `jq`, `grep`, or
    /// offline analysis scripts.
    Dump {
        #[arg(long, default_value = "7d")]
        since: String,
    },
    /// Apply the eval-log maintenance caps from [eval] in config.toml.
    /// Three caps, applied in order:
    ///   1. `max_age_days` (default 30) — drop files older than this.
    ///   2. `max_entries_per_file` (default 5000) — keep the newest N
    ///      lines per file (drop oldest).
    ///   3. `max_session_files` (default 30) — keep the N most-recent
    ///      session files; delete the rest.
    ///
    /// Dry-run by default; pass `--apply` to actually write. Auto-runs
    /// at session_end unless MNEMUSH_EVAL_PRUNE_ON_SESSION_END=off.
    Prune {
        /// Show what would change without writing.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum GraphCmd {
    /// PageRank hub detection. Prints each memory's rank, highest
    /// first. Nodes with more/link-weighted incoming connections
    /// (hubs) score higher — good for "what is the center of this
    /// memory network?"
    Pagerank {
        /// Only print the top N ranked memories (0 = all).
        #[arg(long, short = 'n', default_value_t = 20)]
        top: usize,
    },
    /// Community detection via label propagation. Prints each
    /// community (one line per memory, grouped by shared label).
    /// Densely-linked clusters collapse to a single label; bridges
    /// don't merge them.
    Communities {
        /// Only print communities with at least N members.
        #[arg(long, default_value_t = 1)]
        min_members: usize,
    },
    /// Export the graph for visualization: `--format dot` (Graphviz
    /// digraph) or `--format json` (D3-force {nodes, links}). Use
    /// `--ranks` to annotate nodes with PageRank and `--communities`
    /// to color/group by community.
    Export {
        /// dot | json
        #[arg(long, short = 'f', default_value = "dot")]
        format: String,
        /// Annotate nodes with PageRank (dot: label suffix; json: ignored).
        #[arg(long)]
        ranks: bool,
        /// Color/group nodes by community (dot: color; json: group field).
        #[arg(long)]
        communities: bool,
        /// Write to this file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// `git init` the sync dir + write the current state as an
    /// initial commit. Idempotent: re-running refreshes the
    /// snapshot and amends the initial commit. Add a remote and
    /// `git push` after this.
    Init {
        /// Sync directory (default: ~/mnemush-sync).
        #[arg(long, short = 'd')]
        dir: Option<String>,
    },
    /// Write the current DB state to the sync dir (no git ops).
    Export {
        /// Sync directory (default: ~/mnemush-sync).
        #[arg(long, short = 'd')]
        dir: Option<String>,
    },
    /// Import a sync dir's state into the local DB. Refuses
    /// snapshots from a newer schema_version. Reports per-memory
    /// conflicts (local updated_at newer than snapshot) but leaves
    /// those rows untouched.
    Import {
        /// Sync directory.
        #[arg(long, short = 'd')]
        dir: String,
    },
    /// Push the current DB snapshot to a WebDAV endpoint
    /// (default: 坚果云). Credentials via MNEMUSH_WEBDAV_USER /
    /// MNEMUSH_WEBDAV_PASS env vars.
    WebdavPush {
        /// 自动触发时的 dirty 时间戳: push 成功后仅当 dirty 未被刷新时清除。
        #[arg(long)]
        dirty_ts: Option<i64>,
    },
    /// Pull + merge a WebDAV snapshot into the local DB.
    WebdavPull,
}

#[derive(Subcommand)]
enum IdentityCmd {
    /// Print the current USER.md / PERSONA.md / CONSTITUTION.md contents.
    Show,
    /// List pending identity-update proposals. Default filters to
    /// pending only; pass `--all` to also see approved/rejected history.
    ListPending {
        /// Show all proposals regardless of status.
        #[arg(long)]
        all: bool,
        /// Filter by status (pending|approved|rejected).
        #[arg(long)]
        status: Option<String>,
    },
    /// Propose an update to one of the identity files. The proposal is
    /// written to pending.jsonl; the user reviews with `list-pending`
    /// and applies with `approve` or `reject`.
    Propose {
        /// Target file: USER.md, PERSONA.md, or CONSTITUTION.md.
        target: String,
        /// The content to append (will be wrapped in a dated section).
        content: String,
        /// Why this is being proposed (the LLM's reasoning).
        reason: String,
        /// Evidence count: how many distinct observations support this.
        #[arg(long, default_value_t = 1)]
        evidence: u32,
    },
    /// Approve a pending proposal — appends its content to the target file.
    Approve {
        /// The proposal id (8-char prefix from `list-pending` is enough).
        id: String,
    },
    /// Reject a pending proposal — marked rejected, target file untouched.
    Reject { id: String },
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
    let mut store = Store::open(&db_path)?;

    match cli.cmd {
        Cmd::ImportTree { dir, project } => {
            let d = dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| mnemush::default_data_dir().join("neuropils"));
            std::fs::create_dir_all(&d)?;
            let api = MemoryApi::new(&store, &config);
            let proj = project
                .clone()
                .unwrap_or_else(|| mnemush::neuropils::PROJECT.to_string());
            let s = mnemush::neuropils::import_tree(&api, &d, &proj)?;
            println!(
                "neuropils +{} added, {} updated, {} skipped | mushroom_body edges: {}",
                s.added, s.updated, s.skipped, s.edges
            );
        }
        Cmd::ExportTree { dir, project } => {
            let d = dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| mnemush::default_data_dir().join("neuropils"));
            let api = MemoryApi::new(&store, &config);
            let proj = project
                .clone()
                .unwrap_or_else(|| mnemush::neuropils::PROJECT.to_string());
            let s = mnemush::neuropils::export_tree(&api, &d, &proj)?;
            println!("exported {} memories to {}", s.written, d.display());
        }
        Cmd::Consolidate {
            project,
            dry_run,
            suggest,
            since,
        } => {
            let api = MemoryApi::new(&store, &config);
            let opts = mnemush::consolidate::RunOpts {
                project,
                dry_run,
                suggest,
                since,
                dream: false,
            };
            let (s, n) = mnemush::consolidate::run_consolidate(&api, &opts)?;
            if !dry_run && !suggest {
                println!(
                    "consolidate: {n} candidates | +{} updated, +{} links, +{} merged, +{} insight, -{} decayed, -{} forgot | {} error(s)",
                    s.updated, s.links, s.merged, s.insights, s.decayed, s.forgot, s.errors.len()
                );
                for e in &s.errors {
                    println!("  ⚠ {e}");
                }
            }
        }
        Cmd::Dream {
            project,
            dry_run,
            suggest,
        } => {
            let api = MemoryApi::new(&store, &config);
            let opts = mnemush::consolidate::RunOpts {
                project,
                dry_run,
                suggest,
                since: None,
                dream: true,
            };
            let (s, n) = mnemush::consolidate::run_consolidate(&api, &opts)?;
            if !dry_run && !suggest {
                println!(
                    "dream: {n} candidates | +{} updated, +{} links, +{} merged, +{} insight, -{} decayed, -{} forgot, {} neuropilized | {} error(s), {} warning(s)",
                    s.updated, s.links, s.merged, s.insights, s.decayed, s.forgot, s.neuropilized, s.errors.len(), s.warnings.len()
                );
                for e in &s.errors {
                    println!("  ⚠ {e}");
                }
                for w in &s.warnings {
                    println!("  ! {w}");
                }
            }
        }
        Cmd::Daemon { dry_run } => {
            if dry_run {
                let now = chrono::Local::now();
                let next = mnemush::daemon::next_wake(now, &config.dream.scheduled_time).unwrap_or(now);
                let used = mnemush::daemon::todays_tokens(&mnemush::default_data_dir());
                println!("dry-run:");
                println!("  now            = {}", now.format("%Y-%m-%d %H:%M:%S"));
                println!("  next_wake      = {}", next.format("%Y-%m-%d %H:%M:%S"));
                println!("  scheduled_time = {}", config.dream.scheduled_time);
                println!("  daily_token_budget = {}", config.dream.daily_token_budget);
                println!("  tokens_used_today   = {used}");
                println!("  provider = {}", config.dream.provider);
                println!("  chunked        = {}", config.dream.chunked);
                return Ok(());
            }
            mnemush::daemon::run(&config)?;
        }
        Cmd::WebdavSafetyCheck => {
            let issues = mnemush::webdav::binary_safety_diagnose();
            if issues.is_empty() {
                println!("ok: no stale binaries detected.");
                println!("  (auto-sync safe to enable via [sync] webdav_enabled = true)");
                std::process::exit(0);
            } else {
                println!("FAIL: stale binaries detected:");
                for i in &issues {
                    println!("  - {i}");
                }
                println!();
                println!("Fix: cargo install --path crates/mnemush --force");
                std::process::exit(1);
            }
        }
        Cmd::Device { name } => {
            if let Some(new_name) = name {
                mnemush::device::set_device_name(&new_name)?;
                println!("renamed to: {new_name}");
                return Ok(());
            }
            let info = mnemush::device::current_device_info();
            println!("device id   = {}", info.id);
            println!("device name = {}", info.name);
            println!("id file     = {}", info.id_path.display());
            println!("name file   = {}", info.name_path.display());
            // Per-origin breakdown so the user sees how many memories
            // were created on this device vs others.
            let conn = &store.conn;
            let total_active: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let local_active: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL AND origin_device = ?1",
                    rusqlite::params![&info.id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let null_origin: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL AND origin_device IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let other = total_active - local_active - null_origin;
            println!();
            println!("origin breakdown (active):");
            println!("  {} (this device): {}", info.name, local_active);
            if other > 0 {
                println!("  other devices       : {other}");
            }
            if null_origin > 0 {
                println!("  unknown (pre-v1.6.2): {null_origin}");
                println!("  hint: `mnemush memory reorigin --local` to backfill");
            }
        }
        Cmd::MemoryReorigin { local, device } => {
            let target = if local {
                Some(mnemush::device::local_device_id())
            } else {
                device.clone()
            };
            let target = match target {
                Some(t) if !t.is_empty() => t,
                _ => {
                    eprintln!("error: specify --local or --device <id>");
                    std::process::exit(2);
                }
            };
            let n = store.conn.execute(
                "UPDATE memory SET origin_device = ?1 WHERE origin_device IS NULL",
                rusqlite::params![&target],
            )?;
            println!("reorigin: {n} memory rows stamped with origin_device = {target}");
        }
        Cmd::Concepts { limit, format } => {
            let api = MemoryApi::new(&store, &config);
            let list = mnemush::concepts::concepts(&api, limit)?;
            match format.as_deref() {
                Some("json") => {
                    let n = list.len();
                    println!(
                        "{}",
                        serde_json::json!({"concepts": list, "count": n}).to_string()
                    )
                }
                _ => {
                    for c in &list {
                        println!("· {} ({})", c.title, c.category);
                    }
                }
            }
        }
        Cmd::Search {
            query,
            category,
            limit,
            project,
            all_projects,
        } => {
            let opts = SearchOpts {
                category: category.as_deref().and_then(Category::parse),
                project,
                limit,
                cross_project_override: all_projects,
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
                    println!("       {}", mnemush::truncate(&h.memory.content.replace('\n', " "), 80));
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
                println!("🔗 {} related memory(ies):", r.conflicts.len());
                for c in r.conflicts {
                    println!("  - {} ({})", c.title, c.category.as_str());
                }
            }
            if let Ok(rep) = mnemush::capacity::enforce_capacity(&api) {
                if rep.evicted_wiki + rep.evicted_low > 0 {
                    println!(
                        "容量驱逐: 已清 wiki 索引 {} 条, 低分记忆 {} 条 (库 {:.0}/{:.0} MB)",
                        rep.evicted_wiki, rep.evicted_low, rep.db_mb, rep.limit_mb
                    );
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
        Cmd::List {
            limit,
            category,
            all_projects,
            origin,
        } => {
            let api = MemoryApi::new(&store, &config);
            // --all-projects bypasses MNEMUSH_PROJECT isolation; we
            // call list_in_project with None so the filter doesn't
            // kick in.
            let project_filter = if all_projects {
                None
            } else {
                api.effective_read_filter()
            };
            let mut mems = api.list_in_project(limit, project_filter)?;
            if let Some(c) = category.as_deref().and_then(Category::parse) {
                mems.retain(|m| m.category == c);
            }
            // v1.6.2: --origin filter. Match against id (exact) or name
            // (substring). Use `local` to mean this device's id.
            if let Some(o) = origin.as_deref() {
                let needle: String = if o == "local" {
                    mnemush::device::local_device_id()
                } else {
                    o.to_string()
                };
                let local_id = mnemush::device::local_device_id();
                let local_name = mnemush::device::local_device_name();
                mems.retain(|m| {
                    let dev = m.origin_device.as_deref().unwrap_or("");
                    dev == needle
                        || dev.starts_with(&needle)
                        || (dev == local_id && (needle == local_name || needle == "local"))
                });
            }
            for m in mems {
                let origin_tag = m
                    .origin_device
                    .as_deref()
                    .map(|d| format!(" origin={}…", d.chars().take(8).collect::<String>()))
                    .unwrap_or_else(|| " origin=unknown".to_string());
                println!(
                    "[{}] {} ({})  #{}{}",
                    m.memory_type.as_str(),
                    m.title,
                    m.category.as_str(),
                    &m.id[..8],
                    origin_tag,
                );
                if m.status != mnemush::schema::ActionStatus::Active {
                    println!("      status: {}", m.status.as_str());
                }
                if let Some(d) = &m.due_at {
                    println!("      due:    {}", d);
                }
            }
        }
        Cmd::Delete { id } => {
            let api = MemoryApi::new(&store, &config);
            api.soft_delete(&id)?;
            println!("soft-deleted: {}", id);
        }
        Cmd::Prune {
            apply,
            limit,
            isolate,
            grace_days,
            max_importance,
            min_days_no_access,
        } => {
            let now = chrono::Utc::now();
            if isolate {
                let opts = mnemush::forget::IsolateOpts {
                    grace_days,
                    max_importance,
                    min_days_no_access,
                    limit,
                };
                run_isolate(&mut store, &config, now, apply, opts)?;
            } else {
                run_prune(&mut store, &config, now, apply, limit)?;
            }
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
        Cmd::Status => {
            let active: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let soft_deleted: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            let edges: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory_edge WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let needs_review: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory WHERE needs_review=1 AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            // Use the actual should_prune predicate, not the loose
            // "importance < 0.7" filter — the latter counts memories
            // that are still fresh and high-confidence.
            let now = chrono::Utc::now();
            let prune_candidates = mnemush::forget::prune_dry_run(&store, &config, now, None)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            let api = MemoryApi::new(&store, &config);
            let reflect_n = api
                .reflect_candidates(now, 7, 999)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            let pending_proposals =
                mnemush::identity::list_pending(Some(mnemush::identity::ProposalStatus::Pending))
                    .map(|v| v.len() as i64)
                    .unwrap_or(0);
            println!("mnemush status");
            println!("  memories (active):    {}", active);
            // v1.6.2: per-origin breakdown. Group by origin_device, label
            // the local device with the user's name, others by their id's
            // first 8 chars (no central registry). NULL origin = pre-migration.
            {
                use rusqlite::params;
                let local_id = mnemush::device::local_device_id();
                let local_name = mnemush::device::local_device_name();
                type Row = (Option<String>, i64);
                let rows: Vec<Row> = (|| -> anyhow::Result<Vec<Row>> {
                    let mut stmt = store.conn.prepare(
                        "SELECT origin_device, COUNT(*) FROM memory \
                         WHERE deleted_at IS NULL GROUP BY origin_device \
                         ORDER BY 2 DESC",
                    )?;
                    let v: Vec<Row> = stmt
                        .query_map(params![], |r| {
                            Ok((r.get::<_, Option<String>>(0)?, r.get(1)?))
                        })?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(v)
                })()
                .unwrap_or_default();
                if !rows.is_empty() {
                    print!("    by origin         : ");
                    let mut parts: Vec<String> = Vec::new();
                    let mut local_n: i64 = 0;
                    let mut unknown_n: i64 = 0;
                    let mut others: Vec<(String, i64)> = Vec::new();
                    for (dev, n) in rows {
                        match dev {
                            None => unknown_n = n,
                            Some(d) if d == local_id => local_n = n,
                            Some(d) => others.push((d, n)),
                        }
                    }
                    if local_n > 0 {
                        parts.push(format!("{local_name}({local_n})"));
                    }
                    for (d, n) in others {
                        parts.push(format!("{}…({n})", &d.chars().take(8).collect::<String>()));
                    }
                    if unknown_n > 0 {
                        parts.push(format!("unknown({unknown_n})"));
                    }
                    println!("{}", parts.join(", "));
                }
            }
            println!("  memories (soft-del):  {}", soft_deleted);
            println!("  edges (active):       {}", edges);
            println!("  needs_review:         {}", needs_review);
            println!(
                "  prune candidates:     {} (matches should_prune)",
                prune_candidates
            );
            println!("  reflect candidates:   {} (last 7d)", reflect_n);
            println!(
                "  pending proposals:    {} (run `mnemush identity list-pending`)",
                pending_proposals
            );
            let size = mnemush::capacity::db_size_mb(&store).unwrap_or(0.0);
            let limit = config.capacity.max_db_mb;
            let np_count: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL AND context LIKE 'neuropil:%'",
                [],
                |r| r.get(0),
            )?;
            println!("  capacity:    {size:.0}/{limit:.0} MB (neuropil 入口 {np_count})");
        }
        Cmd::EdgeDecay => {
            let updated =
                mnemush::forget::decay_all_edges(&mut store, &config, chrono::Utc::now())?;
            println!("edges decayed: {updated}");
        }
        Cmd::ProcessNeedsReview { grace_days } => {
            let grace = chrono::Duration::days(grace_days);
            let n = mnemush::forget::process_needs_review(&mut store, grace)?;
            println!("needs_review processed: {n}");
        }
        Cmd::Identity { action } => match action {
            IdentityCmd::Show => {
                let id = Identity::load().unwrap_or_default();
                if id.is_empty() {
                    println!("(no identity files in ~/.mnemush/identity/)");
                    println!("run `mnemush init` to bootstrap");
                } else {
                    println!("{}", id.render_prompt_block());
                }
            }
            IdentityCmd::ListPending { status, all } => {
                let status_filter = if let Some(s) = status {
                    Some(match s.as_str() {
                        "pending" => mnemush::identity::ProposalStatus::Pending,
                        "approved" => mnemush::identity::ProposalStatus::Approved,
                        "rejected" => mnemush::identity::ProposalStatus::Rejected,
                        other => {
                            println!(
                                "unknown status '{}', expected pending|approved|rejected",
                                other
                            );
                            return Ok(());
                        }
                    })
                } else if all {
                    None
                } else {
                    Some(mnemush::identity::ProposalStatus::Pending)
                };
                let proposals = mnemush::identity::list_pending(status_filter).unwrap_or_default();
                if proposals.is_empty() {
                    println!("(no {}proposals)", if all { "" } else { "pending " });
                    return Ok(());
                }
                println!("{} proposal(s):", proposals.len());
                for p in &proposals {
                    let short = if p.id.len() >= 8 { &p.id[..8] } else { &p.id };
                    println!(
                        "  - {}  [{}→{}|ev={}]  {}",
                        short,
                        format!("{:?}", p.status).to_lowercase(),
                        p.target,
                        p.evidence_count,
                        p.content.chars().take(60).collect::<String>()
                    );
                    println!("       id: {}", p.id);
                    println!("       reason: {}", p.reason);
                }
            }
            IdentityCmd::Propose {
                target,
                content,
                reason,
                evidence,
            } => {
                let p = mnemush::identity::propose(&target, &content, &reason, evidence)?;
                let short = if p.id.len() >= 8 { &p.id[..8] } else { &p.id };
                println!(
                    "proposed #{} → {} (run `mnemush identity list-pending` to review, then `approve {}` or `reject {}`)",
                    short, target, short, short
                );
            }
            IdentityCmd::Approve { id } => match mnemush::identity::approve(&id)? {
                Some(p) => {
                    let short = if p.id.len() >= 8 { &p.id[..8] } else { &p.id };
                    println!("approved #{} → appended to {}", short, p.target);
                }
                None => println!("(no pending proposal with id {})", &id[..id.len().min(8)]),
            },
            IdentityCmd::Reject { id } => match mnemush::identity::reject(&id)? {
                Some(p) => {
                    let short = if p.id.len() >= 8 { &p.id[..8] } else { &p.id };
                    println!("rejected #{} (was for {})", short, p.target);
                }
                None => println!("(no pending proposal with id {})", &id[..id.len().min(8)]),
            },
        },
        Cmd::Config => {
            println!("{:#?}", config);
        }
        Cmd::Init => {
            init_dotfiles()?;
        }
        Cmd::Reflect { since_days, limit } => {
            let api = MemoryApi::new(&store, &config);
            let hits = api.reflect_candidates(chrono::Utc::now(), since_days, limit)?;
            if hits.is_empty() {
                println!("(no candidates)");
                return Ok(());
            }
            println!("{} reflection candidate(s):", hits.len());
            for m in &hits {
                let edge_count: i64 = store.conn.query_row(
                    "SELECT COUNT(*) FROM memory_edge \
                     WHERE (source_id=?1 OR target_id=?1) AND deleted_at IS NULL",
                    rusqlite::params![m.id],
                    |r| r.get(0),
                )?;
                println!(
                    "  - {}  [{}|imp={:.2}|edges={}]  {}",
                    short_id(&m.id),
                    m.category.as_str(),
                    m.importance,
                    edge_count,
                    m.title,
                );
                if !m.content.is_empty() {
                    println!("       {}", mnemush::truncate(&m.content.replace('\n', " "), 80));
                }
            }
        }
        Cmd::Eval { action } => {
            use std::collections::BTreeMap;
            match action {
                EvalCmd::Stats { since } => {
                    let cutoff = parse_since_seconds(&since);
                    let eval_dir = mnemush::eval::eval_dir();
                    if !eval_dir.exists() {
                        println!("(no eval data at {})", eval_dir.display());
                        println!("Hint: agent plugins (pi, OpenCode) write per-session");
                        println!("NDJSON to this dir on each tool call. Nothing has been");
                        println!("captured yet.");
                        return Ok(());
                    }
                    let mut total = 0usize;
                    let mut errors = 0usize;
                    let mut by_tool: BTreeMap<String, usize> = BTreeMap::new();
                    let mut lats: Vec<u64> = Vec::new();
                    let mut sessions: BTreeMap<String, usize> = BTreeMap::new();
                    for entry in std::fs::read_dir(&eval_dir)
                        .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?
                    {
                        let entry = entry
                            .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?;
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
                            continue;
                        }
                        let content = std::fs::read_to_string(&path)
                            .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?;
                        for line in content.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else {
                                continue;
                            };
                            if let Some(ts) = e.get("ts").and_then(|v| v.as_i64()) {
                                if ts < cutoff {
                                    continue;
                                }
                            }
                            total += 1;
                            if e.get("error").map(|v| !v.is_null()).unwrap_or(false) {
                                errors += 1;
                            }
                            if let Some(tool) = e.get("tool").and_then(|v| v.as_str()) {
                                *by_tool.entry(tool.to_string()).or_default() += 1;
                            }
                            if let Some(lat) = e.get("latency_ms").and_then(|v| v.as_u64()) {
                                lats.push(lat);
                            }
                            if let Some(s) = e.get("session").and_then(|v| v.as_str()) {
                                *sessions.entry(s.to_string()).or_default() += 1;
                            }
                        }
                    }
                    if total == 0 {
                        println!("(no eval entries in the last {since})");
                        return Ok(());
                    }
                    println!(
                        "self-eval (last {since}): {} total calls across {} session(s)",
                        total,
                        sessions.len()
                    );
                    println!();
                    lats.sort_unstable();
                    let p = |q: f64| -> u64 {
                        if lats.is_empty() {
                            0
                        } else {
                            let i = ((lats.len() as f64 - 1.0) * q) as usize;
                            lats[i]
                        }
                    };
                    let p50 = p(0.50);
                    let p95 = p(0.95);
                    println!(
                        "  latency:    p50={}ms  p95={}ms  (n={})",
                        p50,
                        p95,
                        lats.len()
                    );
                    println!(
                        "  errors:     {} / {} = {:.1}%",
                        errors,
                        total,
                        100.0 * errors as f64 / total as f64
                    );
                    println!();
                    println!("  by tool:");
                    for (tool, count) in &by_tool {
                        let pct = 100.0 * *count as f64 / total as f64;
                        println!("    {:<28} {:>4}  ({:>5.1}%)", tool, count, pct);
                    }
                }
                EvalCmd::Dump { since } => {
                    let cutoff = parse_since_seconds(&since);
                    let eval_dir = mnemush::eval::eval_dir();
                    if !eval_dir.exists() {
                        return Ok(());
                    }
                    for entry in std::fs::read_dir(&eval_dir)
                        .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?
                    {
                        let entry = entry
                            .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?;
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
                            continue;
                        }
                        let content = std::fs::read_to_string(&path)
                            .map_err(|e| mnemush::error::MnemushError::Other(e.to_string()))?;
                        for line in content.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                                continue;
                            };
                            if let Some(ts) = v.get("ts").and_then(|v| v.as_i64()) {
                                if ts < cutoff {
                                    continue;
                                }
                            }
                            println!("{}", v);
                        }
                    }
                }
                EvalCmd::Prune { apply } => {
                    let r = if apply {
                        mnemush::eval::prune_apply(&config.eval)?
                    } else {
                        mnemush::eval::prune_dry_run(&config.eval)?
                    };
                    println!(
                        "{}: {} file(s) kept, {} lines kept; removed by age={}, by count={}, lines dropped={} (≈{} bytes)",
                        if apply { "pruned" } else { "would prune" },
                        r.files_kept,
                        r.lines_kept,
                        r.files_removed_age,
                        r.files_removed_count,
                        r.lines_dropped_count,
                        r.bytes_recovered_estimated,
                    );
                }
            }
        }
        Cmd::Graph { action } => {
            use mnemush::graph;
            let g = graph::Graph::load(&store)?;
            match action {
                GraphCmd::Pagerank { top } => {
                    let ranks = graph::pagerank(&g, 0.85, 100, 1e-6);
                    // Sort by rank desc, tie-break by title.
                    let mut idx: Vec<usize> = (0..g.nodes.len()).collect();
                    idx.sort_by(|&a, &b| {
                        ranks[b]
                            .partial_cmp(&ranks[a])
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| g.nodes[a].title.cmp(&g.nodes[b].title))
                    });
                    if g.active_count() == 0 {
                        println!("(graph is empty — add memories and link them first)");
                        return Ok(());
                    }
                    let shown = idx.iter().take(if top == 0 {
                        idx.len()
                    } else {
                        top.min(idx.len())
                    });
                    for &i in shown {
                        let m = &g.nodes[i];
                        println!("{:>7.4}  #{}  {}", ranks[i], &m.id[..8], m.title);
                    }
                }
                GraphCmd::Communities { min_members } => {
                    let labels = graph::label_propagation(&g, 50);
                    // Group by label.
                    let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
                        std::collections::BTreeMap::new();
                    for (i, l) in labels.iter().enumerate() {
                        groups.entry(l.clone()).or_default().push(i);
                    }
                    if g.active_count() == 0 {
                        println!("(graph is empty — add memories and link them first)");
                        return Ok(());
                    }
                    let mut n = 0usize;
                    for (label, members) in &groups {
                        if members.len() < min_members {
                            continue;
                        }
                        n += 1;
                        println!(
                            "community {} ({}, {} member(s)):",
                            n,
                            &label[..8.min(label.len())],
                            members.len()
                        );
                        for &i in members {
                            let m = &g.nodes[i];
                            println!("    #{}  {}", &m.id[..8], m.title);
                        }
                    }
                    if n == 0 {
                        println!("(no communities with >= {} member(s))", min_members);
                    }
                }
                GraphCmd::Export {
                    format,
                    ranks,
                    communities,
                    output,
                } => {
                    let edges = graph::load_edges(&store)?;
                    let com_opt = if communities {
                        Some(graph::label_propagation(&g, 50))
                    } else {
                        None
                    };
                    let body = match format.as_str() {
                        "dot" => {
                            // export_d3 不收 ranks; 只有 dot 需要算 PageRank。
                            let ranks_opt = if ranks {
                                Some(graph::pagerank(&g, 0.85, 100, 1e-6))
                            } else {
                                None
                            };
                            graph::export_dot(&g, &edges, ranks_opt.as_deref(), com_opt.as_deref())
                        }
                        "json" => graph::export_d3(&g, &edges, com_opt.as_deref()),
                        other => {
                            println!("unknown format '{}' (expected dot|json)", other);
                            return Ok(());
                        }
                    };
                    match output {
                        Some(path) => {
                            std::fs::write(&path, &body).map_err(|e| {
                                mnemush::error::MnemushError::Other(format!(
                                    "write {}: {}",
                                    path, e
                                ))
                            })?;
                            println!("wrote {} ({} bytes)", path, body.len());
                        }
                        None => print!("{}", body),
                    }
                }
            }
        }
        Cmd::Reindex => {
            // Rebuild the FTS5 index from the memory table. FTS rowid
            // must equal memory rowid (search JOINs on it), so we
            // insert ALL rows (incl. soft-deleted) to keep alignment.
            let tx = store.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM memory_fts", [])?;
            tx.execute(
                "INSERT INTO memory_fts(rowid, title, content, context, tags) \
                 SELECT rowid, title, content, context, tags FROM memory",
                [],
            )?;
            tx.commit()?;
            let n: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM memory_fts", [], |r| r.get(0))?;
            println!("reindexed memory_fts ({} rows)", n);
        }
        Cmd::Sync { action } => {
            use mnemush::sync;
            match action {
                SyncCmd::Init { dir } => {
                    let dir = dir.map(std::path::PathBuf::from).unwrap_or_else(|| {
                        std::env::var("HOME")
                            .map(|h| std::path::PathBuf::from(h).join("mnemush-sync"))
                            .unwrap_or_else(|_| std::path::PathBuf::from("./mnemush-sync"))
                    });
                    let m = sync::init_sync(&store, &dir)?;
                    println!(
                        "initialized {} ({} active memories, schema v{}, mnemush {})",
                        dir.display(),
                        m.counts.active_memories,
                        m.schema_version,
                        m.mnemush_version
                    );
                    println!(
                        "next: `cd {} && git remote add origin <url> && git push -u origin main`",
                        dir.display()
                    );
                }
                SyncCmd::Export { dir } => {
                    let dir = dir.map(std::path::PathBuf::from).unwrap_or_else(|| {
                        std::env::var("HOME")
                            .map(|h| std::path::PathBuf::from(h).join("mnemush-sync"))
                            .unwrap_or_else(|_| std::path::PathBuf::from("./mnemush-sync"))
                    });
                    let m = sync::export_to(&store, &dir)?;
                    println!(
                        "exported {} ({} active memories, schema v{})",
                        dir.display(),
                        m.counts.active_memories,
                        m.schema_version
                    );
                }
                SyncCmd::Import { dir } => {
                    let dir = std::path::PathBuf::from(&dir);
                    let r = sync::import_from(&store, &dir)?;
                    println!(
                        "imported {} memories, {} edges, {} embeddings, {} identity file(s)",
                        r.imported, r.edges_imported, r.embeddings_imported, r.identity_copied
                    );
                    if !r.conflicts.is_empty() {
                        println!(
                            "{} conflict(s) left untouched (local is newer):",
                            r.conflicts.len()
                        );
                        for c in &r.conflicts {
                            println!("  - {}", c);
                        }
                        println!("resolve manually (e.g. delete local copy and re-import).");
                    }
                }
                SyncCmd::WebdavPush { dirty_ts } => {
                    mnemush::webdav::push(&store, &mnemush::default_data_dir())?;
                    // 自动触发: push 成功后清除未被刷新的 dirty(守卫防丢同步)。
                    if let Some(ts) = dirty_ts {
                        let d = mnemush::default_data_dir();
                        let dirty = d.join("sync-dirty");
                        if std::fs::read_to_string(&dirty)
                            .map(|c| c.trim() == ts.to_string())
                            .unwrap_or(false)
                        {
                            let _ = std::fs::remove_file(&dirty);
                        }
                    }
                    println!("webdav push ok");
                }
                SyncCmd::WebdavPull => {
                    let r = mnemush::webdav::pull(&store, &mnemush::default_data_dir())?;
                    println!(
                        "imported {} memories, {} conflicts, {} edges skipped",
                        r.imported,
                        r.conflicts.len(),
                        r.skipped_edges
                    );
                }
            }
        }
        Cmd::Backup {
            output,
            include_eval,
        } => {
            let data_dir = mnemush::default_data_dir();
            let out = match output {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                    std::path::PathBuf::from(home).join(format!("mnemush-backup-{}.tar.gz", ts))
                }
            };
            let meta = mnemush::backup::create_backup_to(&data_dir, &out, include_eval)?;
            println!(
                "backup: {} (schema_version={}, memories={}, edges={})",
                out.display(),
                meta.schema_version,
                meta.counts.active_memories,
                meta.counts.edges,
            );
        }
        Cmd::Restore {
            input,
            target,
            yes,
            force,
        } => {
            let target_dir = match target {
                Some(t) => std::path::PathBuf::from(t),
                None => mnemush::default_data_dir(),
            };
            let archive = std::path::PathBuf::from(&input);
            if !yes {
                println!(
                    "This will overwrite the contents of {} with data from {}.",
                    target_dir.display(),
                    archive.display()
                );
                println!("Type 'yes' to continue, anything else to abort:");
                let mut line = String::new();
                std::io::stdin()
                    .read_line(&mut line)
                    .map_err(|e| mnemush::error::MnemushError::Other(format!("stdin: {}", e)))?;
                if line.trim() != "yes" {
                    println!("aborted");
                    return Ok(());
                }
            }
            let meta = mnemush::backup::restore_backup_to(&archive, &target_dir, force)?;
            println!(
                "restored {} (schema_version={}, memories={}, edges={})",
                target_dir.display(),
                meta.schema_version,
                meta.counts.active_memories,
                meta.counts.edges,
            );
        }
        Cmd::Embed {
            title_contains,
            force,
        } => {
            if !config.embedding.enabled {
                println!(
                    "[mnemush] embeddings not enabled — set `[embeddings] enabled = true` \
                     in ~/.mnemush/config.toml."
                );
                return Ok(());
            }
            let model = config.embedding.model.clone();
            println!(
                "[mnemush] loading model {} (downloads on first run)...",
                model
            );
            let mut emb = mnemush::embeddings::Embedder::new(&model)?;
            let api = mnemush::memory::MemoryApi::new(&store, &config);
            let mems = api.list(10_000)?;
            let target: Vec<&mnemush::schema::Memory> = mems
                .iter()
                .filter(|m| match &title_contains {
                    Some(s) => m.title.to_lowercase().contains(&s.to_lowercase()),
                    None => true,
                })
                .filter(|m| {
                    if force {
                        return true;
                    }
                    // Skip memories that already have an embedding for
                    // this model. The `Result<Option<_>> → bool`
                    // dance collapses to "true when the embedding
                    // is absent".
                    store
                        .get_embedding(&m.id, emb.model_id())
                        .ok()
                        .flatten()
                        .is_none()
                })
                .collect();
            if target.is_empty() {
                println!("(no matching memories)");
                return Ok(());
            }
            // Embed title + content concatenated (best signal), batched so a
            // hung/failed API call keeps previously embedded memories
            // (resume-safe; a later run skips memories that already have one).
            let mut count = 0usize;
            for chunk in target.chunks(100) {
                let texts: Vec<String> = chunk
                    .iter()
                    .map(|m| format!("{} {} {}", m.title, m.content, m.tags.join(" ")))
                    .collect();
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                let vectors = emb.embed(&refs)?;
                let tx = store.conn.unchecked_transaction()?;
                for (mem, vec) in chunk.iter().zip(vectors.iter()) {
                    mnemush::embeddings::put_embedding_tx(
                        &tx,
                        &mem.id,
                        emb.model_id(),
                        emb.dim() as i64,
                        vec,
                    )?;
                    count += 1;
                }
                tx.commit()?;
                eprintln!("[mnemush] embedded {} so far...", count);
            }
            println!(
                "embedded {} memory(ies) with model {} ({}d, {} bytes each)",
                count,
                emb.model_id(),
                emb.dim(),
                emb.dim() * 4
            );
        }
    }
    Ok(())
}

/// Parse a duration like "1d", "12h", "30m" into a unix-seconds cutoff.
/// Default to 0 (include everything) for unrecognized forms — the
/// caller will just see a large window, not silently drop data.
fn parse_since_seconds(s: &str) -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let split_at = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split_at);
    let Ok(n) = num.parse::<i64>() else {
        return 0;
    };
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        "w" => n * 604800,
        _ => return 0,
    };
    now.saturating_sub(secs)
}

fn print_memory(m: &mnemush::schema::Memory) {
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
    println!("status:      {}", m.status.as_str());
    if let Some(d) = &m.due_at {
        println!("due_at:      {}", d);
    }
    if let Some(c) = &m.claimed_by {
        println!("claimed_by:  {}", c);
    }
    if let Some(p) = &m.parent_id {
        println!("parent_id:   {}", p);
    }
    if let Some(c) = &m.completed_at {
        println!("completed_at:{}", c);
    }
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

fn run_prune(
    store: &mut Store,
    cfg: &Config,
    now: chrono::DateTime<chrono::Utc>,
    apply: bool,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    if apply {
        let deleted = forget::prune_apply(store, cfg, now, limit)?;
        if deleted.is_empty() {
            println!("(no candidates)");
            return Ok(());
        }
        println!("soft-deleted {} memory(ies):", deleted.len());
        for (id, reason) in deleted {
            println!("  - {}  [{}]", short_id(&id), reason_label(&reason));
        }
    } else {
        let hits = forget::prune_dry_run(store, cfg, now, limit)?;
        if hits.is_empty() {
            println!("(no prune candidates)");
            return Ok(());
        }
        println!("DRY RUN: would soft-delete {} memory(ies):", hits.len());
        for (m, reason) in hits {
            println!(
                "  - {}  [{:>5.2}]  {}  ({})",
                short_id(&m.id),
                m.importance,
                m.title,
                reason_label(&reason)
            );
        }
        println!("\nrerun with --apply to soft-delete; recover via custom UPDATE setting deleted_at=NULL");
    }
    Ok(())
}

fn run_isolate(
    store: &mut Store,
    cfg: &Config,
    now: chrono::DateTime<chrono::Utc>,
    apply: bool,
    opts: mnemush::forget::IsolateOpts,
) -> anyhow::Result<()> {
    if apply {
        let deleted = forget::isolate_hard_delete(store, cfg, now, opts)?;
        if deleted.is_empty() {
            println!("(no isolated candidates)");
            return Ok(());
        }
        println!("hard-deleted {} memory(ies):", deleted.len());
        for (id, reason) in deleted {
            println!("  - {}  [{}]", short_id(&id), reason_label(&reason));
        }
    } else {
        let hits = forget::isolate_dry_run(store, now, opts)?;
        if hits.is_empty() {
            println!("(no isolated candidates)");
            return Ok(());
        }
        println!("DRY RUN: would hard-delete {} memory(ies):", hits.len());
        for m in hits {
            let days_no_access = (now.timestamp() - m.last_accessed_at.timestamp()) / 86_400;
            println!(
                "  - {}  [{:>5.2}]  {}d no access, grace={}d",
                short_id(&m.id),
                m.importance,
                days_no_access,
                opts.grace_days
            );
        }
        println!("\nrerun with --apply to hard-delete; this is irreversible");
    }
    Ok(())
}

fn reason_label(r: &PruneReason) -> String {
    match r {
        PruneReason::LowConfidence {
            confidence,
            threshold,
            days_no_access,
        } => {
            format!(
                "low_conf conf={:.3} < {:.3}, {}d",
                confidence, threshold, days_no_access
            )
        }
        PruneReason::Stale {
            confidence,
            threshold,
            days_no_access,
        } => {
            format!(
                "stale conf={:.3} < {:.3}, {}d",
                confidence, threshold, days_no_access
            )
        }
        PruneReason::Isolated {
            grace_days,
            importance,
            max_importance,
            days_no_access,
            ..
        } => {
            format!(
                "isolated imp={:.2} < {:.2}, grace={}d, {}d no access",
                importance, max_importance, grace_days, days_no_access
            )
        }
    }
}

/// Show the first 8 chars of a UUID-style id safely (some test fixtures
/// use shorter strings).
fn short_id(id: &str) -> String {
    if id.len() <= 8 {
        id.to_string()
    } else {
        id[..8].to_string()
    }
}

fn init_dotfiles() -> anyhow::Result<()> {
    // init must respect MNEMUSH_DATA_DIR like the rest of the codebase;
    // the previous hard-coded $HOME/.mnemush ignored the env override,
    // making `MNEMUSH_DATA_DIR=... mnemush init` pollute the real home dir.
    let data_dir = mnemush::default_data_dir();
    let id_dir = data_dir.join("identity");
    std::fs::create_dir_all(&id_dir)?;

    // Copy config example
    let config_dst = data_dir.join("config.toml");
    if !config_dst.exists() {
        std::fs::write(
            &config_dst,
            include_str!("../../assets/config.example.toml"),
        )?;
        println!("wrote {}", config_dst.display());
    } else {
        println!("(skipped, exists) {}", config_dst.display());
    }

    // Copy identity templates
    for (name, body) in [
        ("USER.md", include_str!("../../assets/identity/USER.md")),
        (
            "PERSONA.md",
            include_str!("../../assets/identity/PERSONA.md"),
        ),
        (
            "CONSTITUTION.md",
            include_str!("../../assets/identity/CONSTITUTION.md"),
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
