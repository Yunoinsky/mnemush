# Changelog

## v1.5.0 (2026-08-14)

### Added

**DeepSeek Harness (DSH) 插件 —— `packages/mnemush-dsh`。** 原生 Cordis 插件
(与 `@deepseek-ai/dsh-tool-bash` 同契约), 让 mnemush 记忆层在 DSH 里以一等
工具形态工作。

- 注册全部 16 个 memory 工具(`memory_add` / `memory_search` / `memory_get` /
  `memory_link` / `memory_neighbors` / `memory_reflect` /
  `memory_save_search_result` / `mnemush_status` / `memory_next` /
  `memory_frontier` / `memory_action_create` / `memory_action_update` /
  `identity_propose` / `identity_list_pending` / `identity_approve` /
  `identity_reject`), 复用 `mnemush-client` spawn `mnemush-mcp`。
- **概念表注入**: `ctx.systemPrompt.section`(order 90)注入
  `[memory index] N concepts`, 启动 + `session/created` + memory 写入后刷新;
  刷新链串行化避免并发 dispose/register 撞重复 section 名。
- **session 维护**: `session/disposed` 时跑 prune / edge-decay /
  process-needs-review / eval-prune(沿用 Pi 插件的 `MNEMUSH_*_ON_SESSION_END`
  开关; 硬删 `--isolate` 永不自动执行)。
- 插件 duck-type DSH 服务接口(同 mnemush-pi duck-type Pi SDK), 无
  `@deepseek-ai/*` 构建期硬依赖; 安装:`dsh plugin --profile <name> add mnemush-dsh`
  后在 profile 的 `cordis.patch.yml` 加 `- id: mnemush` / `name: mnemush-dsh`。

## v1.4.0 (2026-08-07)

### Added

**概念表(context priming index)—— agent 会话唤起索引。** 给 agent 注入记忆库的
top 概念摘要(零 LLM), 让其在会话开始时就知道记忆库里有什么可搜。

- `mnemush concepts [--limit N] [--format json]` — 活跃记忆按
  importance × recency × access 排序取 top-N, title 压缩(剥前缀 + 48 字符截断)。
- **pi 插件注入**: `session_start` 连接成功后 fire-and-forget 调
  `mnemush concepts --limit 40 --format json`, 以 `[memory index] N concepts`
  custom message 注入 agent 上下文(不刷聊天 UI); `after_tool_call` /
  `tool_result` 检测 memory 写入(memory add/save/action) → 刷新注入。
  失败/空静默, 不阻塞会话。

## v1.3.0 (2026-08-07)

### Added

**容量管理 —— 100MB 物理触发阈值 + 驱逐链 + neuropil 化 + 压缩(dream 三合一)。**
DB 物理大小(`page_count × page_size`)超 `[capacity] max_db_mb`(默认 100MB)即触发驱逐链;驱逐收敛按逻辑活数据估算(内容 + 活跃向量 + 活跃边)判断,驱逐时 VACUUM 回收物理空间。

- **驱逐链**(add 触发, 超限才进入): ① 清可再生的外部知识库动态索引(wiki project 层)→
  ② 仍超 → 按价值/成本评分(importance×confidence×时效 / 成本)软删低分记忆
  (importance≥0.7 / never_prune / identity 豁免)→ ③ VACUUM 回收物理空间。
  估算按活跃数据(内容 + 活跃向量 + 活跃边),驱逐后必收敛。
- **摘要入口**(neuropil 化): 记忆降级为前 2 句摘要 + `context=neuropil:<path>`
  路径标记,全文移出主库;FTS 同步更新(旧全文不再命中),可恢复。
- **neuropil 化动作**: `mnemush dream` 中 LLM 复核规则初筛候选
  (category=note/skill 且非摘要入口),输出 `neuropilize` 动作 → 原内容落盘
  `~/.mnemush/neuropils/<path>`(frontmatter + 全文)+ 主库降级摘要入口。
- **neuropil 压缩**(dream 尾部): 双条件冷判定(入口 30 天无命中 + 文件 30 天
  未改)→ 按 project 合并归档页 → tar.gz 打包移出活动区。
  注: 当前实现是**归档副本 + 摘要索引**——全文副本移入 tar.gz 归档(全文不丢),
  主库保留摘要入口供检索/恢复, 原记忆不删除; **原文删除 + 全文入包**(压缩后
  主库连摘要一并清除, 检索直接走压缩包)列为后续项。
- **dream 三合一**: `mnemush dream` = 遗忘巩固 + neuropilize 复核 + 冷压缩 +
  容量报告(结束打印 `容量: X/Y MB`, Y = `[capacity] max_db_mb`)。
- `mnemush status` 新增 capacity 行: `capacity: X/Y MB (neuropil 入口 N)`。

### Fixed

- neuropilize 之前只降级主库不落盘文件树(导出步骤缺失)→ 逐条写文件到
  neuropils 文件树;写失败(含路径越界)记录错误,不阻塞降级动作。

### Added(后续)

- **dream 采样滚动覆盖**: 10 种子(5 最新 + 5 全库随机)+ 每种子 2 级图延伸
  (每级 ≤`[capacity] dream_sample_m` 随机邻居, 默认 3)→ 每轮 ≤10·m·m 条
  (去重后实际更少)。不再卡在最新批次, 随机+图延伸滚动覆盖全库(含 wiki
  记忆, 由 LLM 决定 forget/neuropilize)。零依赖 SplitMix64 PRNG。
- **LLM 用量采集**: `dream`/`consolidate` 结束后打印
  `llm: N prompt + M completion (推理 R), 合计 T tokens`;LLM 原始响应存档
  附带 usage。LLM 超时 60s→180s、max_tokens 16000→65536(推理模型 +
  采样候选多)。
- **embeddings key 兜底**: `minimax-embo-01` 后端 key 支持
  `MINIMAX_CN_API_KEY`/`MINIMAX_TOKEN_PLAN_KEY`/`~/.mmx/config.json`
  (与 llm 一致, 不再只认 `MINIMAX_API_KEY`)。
- **驱逐报告**: dream 尾部驱逐不再静默, 报告 `(驱逐: wiki 索引 N 条, 低分 M 条)`。


## v1.2.0 (2026-08-07)

### Added

**consolidate — LLM 驱动的记忆巩固 + 主动遗忘。** 参考 Karpathy LLM Wiki
策略(增量编译)+ 钟毅团队主动遗忘研究(独立于记忆形成的主动擦除)+
睡眠期巩固/顿悟。

- `mnemush consolidate [--dry-run|--suggest] [--project <name>] [--since <ts>]`:
  收集自上次以来的新记忆 → LLM(MiniMax M3,quota 耗尽 fallback DeepSeek)
  输出 JSON 动作 → 自动执行。
- **巩固动作**: update(修订)/ link(建边)/ merge(合并重复, 边重定向)/
  insight(顿悟: 跨簇新模式 → insight 记忆 + 边)。
- **主动遗忘动作**(生物映射): decay(confidence 降权, 突触削弱类比,
  下限 0.05, 渐进) / forget(软删);双阈值(低 confidence 易遗忘,
  高 confidence 需明确矛盾/过时证据)。
- **保护规则**(Raf/MAPK 类比): importance≥0.7 / never_prune / identity /
  7 天内创建 → 禁止 decay/forget。
- **遗忘痕迹(forget-trace)**: forget 本身也是信息 —— 每次主动遗忘留下
  forget_trace 元记忆(被遗忘者/时间/摘要/原因),可检索可分析;
  trace 可被未来 dream 再遗忘(不设保护),且不产生 trace-of-trace(防递归)。
- **增量位置**: ~/.mnemush/consolidate.json(last_ts),重跑幂等;
  dream 全量扫描不推进位置(独立调度)。
- **审计**: memory_event(consolidate_*)+ LLM 原始响应存档
  ~/.mnemush/eval/consolidate-<ts>.json。
- `mnemush dream [--dry-run|--suggest] [--project]`: 每日全量巩固,
  更强遗忘强度(睡眠期高峰类比),建议 cron 每日一次。

### Fixed

- LLM 短 id(前 8 字符)传入精确匹配的软删/更新 → 静默失败;所有动作
  先 resolve 成完整 UUID 再执行,并补短 id 回归测试。
- UUID v7 同毫秒创建前缀相同 → `LIKE` 前缀解析可能命中错误记忆;多匹配
  时保守跳过,不冒险猜。
- merge 边重定向撞 `UNIQUE(source,target,type)`(auto-link 同目标边)→
  去重重定向,不再崩溃。
- `--dry-run` 推进增量位置(候选未处理却被跳过)→ dry-run/suggest/dream
  均不写状态。
- 防重复参数(temperature 0.7 + frequency/presence penalty 0.3)缓解
  MiniMax M3 官方确认的循环/重复问题;prompt 候选展示完整 id;
  支持 `MINIMAX_CN_API_KEY` / `MINIMAX_TOKEN_PLAN_KEY` 环境变量。


## v1.1.0 (2026-08-07)

### Added

**Neuropils — 文件树记忆(文件=源, mnemush=关系层)。** 记忆内容以
markdown 文件树为权威源(Agent 可用 grep/cat/tree 直接读、Git 版本化),
`import-tree` 增量同步进 SQLite(mushroom_body:FTS + 向量 + 边)。

- `mnemush import-tree [dir] --project <name>`: 扫描目录树(默认
  `~/.mnemush/neuropils/`),按 title 增量 add/update(幂等,编辑即更新)。
  任意目录树都是 neuropil(既有知识库即实例)。
- `mnemush export-tree [dir] --project <name>`: 记忆落盘为
  `<category>/<title>.md` 文件树(往返稳定)。
- **mushroom_body 边**: frontmatter `links:` + 正文 `[[wikilink]]` 与
  wiki 风格 `[label](path)` → related/supports 边(provenance
  `neuropil:wikilink`);同目录共现 → weak related(`neuropil:copath`)。
- 文件格式: frontmatter(title/category/tags/links)+ 正文;无 frontmatter
  时用文件名作 title。
- 命名映射: neuropils=皮层子脑区(内容层), mushroom_body=海马/蘑菇体
  (跨簇关联索引)。设计见 docs/superpowers/specs/2026-08-07-file-tree-memory-design.md。


## v1.0.1 (2026-08-07)

### Added

**Semantic recall (v1.0, opt-in).** Previously the embedding blend
only re-ranked FTS hits — queries with zero lexical overlap (e.g. a
Chinese query against English memories) returned nothing. Now the
full-corpus vector top-K joins the FTS candidate set before the
BM25+cosine blend, so semantically-similar memories surface even
without shared tokens.

- `search()` pulls `all_embeddings(model)` cosine top-K
  (`limit*2` extra candidates) into the merged candidate set.
- Falls back to BM25-only when embeddings are disabled / model
  can't load / no vectors stored (unchanged behavior).

**scripts/benchmark.py (self-contained retrieval benchmark).**
Isolated DB, 5-cluster x 4-memory corpus (20 memories), 17 queries
with ground-truth relevance (incl. semantic-only Chinese queries),
metrics recall@k / MRR / nDCG@k for k=1,3,5, plus extra checks
(add-dedup, delete-visibility, reindex-idempotence). Run:

    python3 scripts/benchmark.py                 # fts vs semantic
    python3 scripts/benchmark.py --mode fts      # BM25 only

Measured on the corpus (k=5): MRR 0.74 (fts) → 0.93 (semantic);
recall@5 0.76 → 0.94; nDCG@5 0.72 → 0.90.

## v1.0.0 (2026-08-06) ✅ Released

**Stable.** v1.0 ships: API stability surface (3 docs layers),
optional semantic search (blended BM25 + cosine), full v0.x test
suites, schema v4 with auto-migration. Cross-platform CI and
publish-to-registry remain ROADMAP items for v1.x.

### Added

**Auto-merge of near-duplicate memories (v1.0).** When adding a
note/skill/insight/episodic memory whose content is Jaccard-similar
(>= `auto_merge_min_sim`, default 0.6) to an existing memory, the
OLD one is soft-deleted and its edges retargeted to the NEW one.

Catches the failure mode that exact content-hash dedup can't:
repeated captures of an evolving document (e.g. SKILL.md) where a
one-word change bypasses the hash. Decision/Correction/Preference
categories are untouched — they keep the supersede-edge behavior.

- Config: `[edges] auto_merge_enabled` (default true) and
  `auto_merge_min_sim` (default 0.6, stricter than supersede's 0.5).
- Merge is transactional: edge retarget (with self-loop cleanup),
  soft-delete old + its FTS5 row, `memory_auto_merge` audit event.
- 3 unit tests: near-dup note consolidates, decisions don't merge
  (supersede edge instead), merged edges are retargeted.

**Cross-machine sync (v1.0).** Git as the transport, mnemush as
the codec.

- `mnemush sync init [-d DIR]` — `git init` + first snapshot commit.
- `mnemush sync export [-d DIR]` — write current DB state to a sync
dir (no git ops).
- `mnemush sync import -d DIR` — read a sync dir into the local DB.
Refuses snapshots from newer schema_version; reports per-memory
conflicts (local updated_at newer than snapshot) but leaves those
rows for manual resolution.

Sync dir layout: `MANIFEST.json` (schema_version + counts),
`memory.json` (all rows), `edges.json`, `identity/` (verbatim
USER/PERSONA/CONSTITUTION + pending.jsonl), `embeddings/` (one JSON
per (model, memory_id)).

New module `crates/mnemush/src/sync.rs`: `Manifest`, `Counts`,
`export_to`, `init_sync`, `import_from`, `ImportReport`. 4 new unit
tests (round-trip, git repo creation, newer-schema refusal, local-
newer conflict detection).

**Fixed: fresh-DB migration was broken.** `Store::migrate`'s fresh-
DB arm ran the migration registry but used `INSERT OR REPLACE INTO
schema_version` — since `version` is the PK, different versions
never conflicted, so the table accumulated rows 2,3,4 and `SELECT
version` read the first (= 2). Now the first migration INSERTs, the
rest UPDATE the single row. (Bug surfaced by the new sync tests —
their fresh in-memory DBs reported schema_version 2 instead of 4.)

**Cross-platform CI (v1.0).** `windows-latest` added to the
rust, ts, and mcp-smoke jobs. All `run:` steps explicitly set
`shell: bash` for cross-OS consistency. The mcp-smoke job now
also installs Python 3.11 via `setup-python@v5` so
`scripts/test-mcp.py` finds `python3` on Windows. Strategy uses
`fail-fast: false` on the mcp-smoke matrix so a single-OS failure
doesn't hide the others.

**Search blend with embeddings (v1.0).** When
`[embedding] enabled = true`, `MemoryApi::search` blends cosine
similarity over a sentence-transformer embedding with the BM25
score. Final score = `bm25_weight * norm(bm25) + embed_weight * cosine`,
where `norm(bm25)` min-max-scales the BM25 within the hit set so
both signals live in `[0, 1]`. Falls back silently to BM25-only
when embeddings are disabled or no embeddings are stored yet.

- New `cached_embedder(model_id)` in `crates/mnemush/src/embeddings.rs`
  uses `OnceLock<Mutex<HashMap<String, Arc<Mutex<Embedder>>>>>` to
  load the heavy model at most once per process. The fastembed
  `embed()` requires `&mut self`, hence the inner `Mutex`.
- `Store::embeddings_for(memory_ids, model)` returns the stored
  embeddings for a specific id set (used by the blend path; avoids
  loading the entire `all_embeddings` set when only a few hits need
  scoring).
- 2 new tests in `memory.rs`: `search_with_embeddings_disabled_still_works`
  (regression — disabled branch is hit, score field unchanged) and
  `cosine_for_blend_search` (cosine math sanity: identical = 1,
  orthogonal = 0, zero-norm = 0).

**Optional semantic search (v1.0, foundation).** Off by default.
When `[embedding] enabled = true`, `mnemush embed` downloads a
sentence-transformer model (default all-MiniLM-L6-v2 quantized,
~25 MB) to `~/.mnemush/models/` and stores per-memory embeddings in
the new `memory_embedding` table.

- New module `crates/mnemush/src/embeddings.rs` — `Embedder`
  (loads + caches model), `cosine()` (vector similarity), `top_n_cosine()`
  (brute-force ranker), `put_embedding_tx()` / `Store::get_embedding()`
  / `all_embeddings()` / `embeddings_for()` / `count_embeddings()`
  (SQLite accessors).
- New migration `V3ToV4` (idempotent) adds the `memory_embedding`
  table; `SCHEMA_VERSION` bumped to 4. Verified against the live
  home DB — schema_version auto-bumped from 3 → 4 on first open.
- Config: new `[embedding]` section (`enabled`, `model`,
  `bm25_weight`, `embed_weight`); `Config::EmbeddingConfig` +
  `Default`. Default blend: 0.7 BM25 / 0.3 cosine.
- CLI: `mnemush embed [--title-contains PAT] [--force]` backfills
  embeddings for matching active memories. `--force` re-embeds even
  when an embedding for the configured model already exists.
- `fastembed = "5"` as a new Cargo dependency. Builds ONNX Runtime
  from source via `ort-download-binaries-native-tls` (no system dep).
- 7 new unit tests: cosine identities, ranking, exclude filter,
  store/retrieve roundtrip, UPSERT overwrite.

### Added

**Documentation site (v1.0 docs).** Three layers, all published:

- **Rust API** — auto-publishes to [docs.rs/mnemush](https://docs.rs/mnemush) on every crates.io release. Driven by `Cargo.toml` metadata (`description`, `license`, `repository`, `keywords`, `categories`) — all present. Local: `cargo doc --manifest-path crates/mnemush/Cargo.toml --no-deps --open`.
- **TypeScript API** — typedoc-driven, `npm run docs:ts` → `target/docs/typedoc/index.html`. Config in `typedoc.json` (skips bundled-TS internal typecheck, excludes private members, categorizes by group).
- **Conceptual docs** — markdown served directly by GitHub: README, ARCHITECTURE, ROADMAP, CHANGELOG, decisions, config example, RELEASING. Already complete; nothing to publish.

README gained a "Documentation (v1.0)" section pointing at all three.

### Changed

- `#![warn(missing_docs)]` in `crates/mnemush/src/lib.rs` enforces /// on public items. New pub items without docs trigger CI warnings; existing field-level gaps are deferred (warning, not deny).
- `target/docs` is git-ignored (typedoc + cargo doc output).

## v0.4.0 (2026-08-05)

### Fixed

**Multi-project isolation (v0.4).** Opt-in via env. Backward-compatible: with both `MNEMUSH_PROJECT` unset, behavior matches v0.3.

### Added

**Schema migration trait (`Migration`).** v0.1 → v0.2 (`V1ToV2`) and v0.2 → v0.3 (`V2ToV3`) are now individual `Migration` impls in `crates/mnemush/src/migrations.rs`. `Store::migrate` walks the registry in order. Adding a v0.3 → v0.4 migration: write a struct impl'ing `Migration` and append to `default_registry()` — no `Store::migrate` changes needed. Migrations are idempotent (pragma_table_info guards) so re-running on a half-migrated DB is safe. 3 new unit tests: `registry_ends_at_schema_version` (catches "forgot to bump SCHEMA_VERSION"), `migrations_are_idempotent` (re-running is no-op), `registry_runs_in_registered_order`.

**Multi-project isolation (v0.4).** Opt-in via env. Backward-compatible: with both `MNEMUSH_PROJECT` unset, behavior matches v0.3.

- `MNEMUSH_PROJECT=foo` — auto-tag all new memories with `project=foo`; reads (search, list, `memory_next`, `memory_frontier`) are scoped to that project.
- `MNEMUSH_ALL_PROJECTS=1` (or `--all-projects` on `mnemush search`/`mnemush list`) — opt-in escape hatch: reads ignore the project filter.
- `SearchOpts.cross_project_override` carries the CLI flag through (TS client never sets it; agent surface unchanged).
- `Config::project = ProjectConfig { default_project, cross_project_search }`; `apply_env_overrides` wires the env vars.
- 3 unit tests in `memory.rs`: `project_isolation_auto_tags_writes_and_filters_reads`, `cross_project_search_bypasses_isolation`, `no_default_project_is_backward_compatible`.

**Backup / restore (`mnemush backup`, `mnemush restore`).** Round-trip the entire `~/.mnemush/` data directory through a single gzipped tar archive.

- `mnemush backup [-o FILE] [--include-eval]` — produces `<UTC-timestamp>.tar.gz` in `$HOME` by default. The `mnemush.db` inside is captured via the SQLite online backup API so WAL state is consistent (no need for `--checkpoint`). Manifest is the first entry, recording `mnemush_version`, `schema_version`, and live counts. Optional `eval/` inclusion (default off — eval NDJSON is regenerable).
- `mnemush restore -i FILE [--target DIR] [--force] [--yes]` — unpacks into `~/.mnemush` by default. Prompts for `yes` confirmation. Refuses to overwrite a target whose `schema_version` is newer than the backup's; pass `--force` to override. `safe_join()` rejects `..` segments and absolute paths in the archive so a hostile tar can't escape `target_dir`.
- New module `crates/mnemush/src/backup.rs`: `BackupMeta`, `Counts`, `create_backup_to`, `restore_backup_to`, `snapshot_meta`. 4 unit tests (round-trip, downgrade refusal, eval round-trip, unsafe-path rejection).

## v0.3.0 (2026-08-05)

### Added

**Graph analytics over the memory network (`mnemush graph`).** Completes the v0.3 graph-intelligence work.

- `mnemush graph pagerank [-n N]` — PageRank hub detection. Nodes with more incoming/weighted links score higher; prints ranks descending. Standard damping 0.85, dangling-node mass redistribution.
- `mnemush graph communities [--min-members N]` — community detection via label propagation. Deterministic (ties broken by smallest label).
- `mnemush graph export -f dot|json [-o FILE] [--ranks] [--communities]` — Graphviz DOT or D3-force JSON export, optionally annotated with PageRank (dot: label suffix) and/or community (dot: color; json: group).

New `crates/mnemush/src/graph.rs` module: in-memory graph load, PageRank, label propagation, DOT/JSON serializers. 4 unit tests (hub-outranks-leaf, two-communities, DOT shape, D3 JSON shape).

### Fixed

**Tool allowlists in the Pi extension rotted (3 places).** The insight-nudge counter missed OpenCode's v0.3 tools; the self-eval logger duplicated `memory_save_search_result`; the tool-failure capturer only skipped `mnemush-`-prefixed names so Pi's unprefixed `memory*` tools leaked "tool failure" memories. Worse, the OpenCode names used underscores (`mnemush-memory_search`) while OpenCode registers hyphens (`mnemush-memory-search`) — none matched. Fixed with a single `isMnemushTool()` prefix-match helper (shared via mnemush-client) used by all three hooks. Regression test added.

**OpenCode plugin never wrote self-eval logs**, so `mnemush eval stats` only covered Pi sessions. Instrumented the tool-registration chokepoint (`registerTool` wraps every execute) so all 16 tools — including the 3 with hand-written try/catch that bypassed `tryRun` — now write `~/.mnemush/eval/<session>.ndjson`. 3 new tests.

**CLI `mnemush get`/`mnemush list` didn't show v0.3 lifecycle fields** (status, due_at, completed_at, claimed_by, parent_id). Added.

**`call_memory_link` used `unwrap()` after `is_none()` checks** (panic risk on malformed input). Rewrote with let-else destructuring.

### Removed

- `evalArgsCache` / `setLastArgs` in mnemush-pi (write-only, never read)
- `ForgettingConfig::importance_default` (defined, never consumed)

### Changed

- CLI `eval stats`/`eval dump` now use `eval::eval_dir()` (single source of truth for MNEMUSH_DATA_DIR).
- Eval-log mtime fallback: unreadable mtime is treated as "now" instead of epoch (over-keep beats nuking live data).

### Fixed

**MCP input validation now rejects out-of-range and unknown enum values.**

`call_memory_add` and `call_memory_save_search_result` previously accepted `importance > 1` or `importance < 0` (and NaN), letting callers poison the decay formula. Now both reject out-of-range values with a clear `importance must be in [0.0, 1.0] (got X)` error. `call_memory_link` gets the same `strength` check. Helper `range_error(field, value, min, max)` in `bin/mcp.rs` is the single source of truth.

`call_memory_link` previously silently coerced unknown `edge_type` strings to `Related`. Now rejects them: `unknown edge_type: "foo" (must be one of related, supports, contradicts, supersedes)`. `call_memory_add` similarly rejects unknown `category` and `memory_type`. The argument "we don't want to lose data silently" was already the policy in v0.1 (test `unknown_category_errors_instead_of_silent_fallback`), but the implementation still wrote Note/Semantic. Now the implementation matches the policy.

`call_memory_link` previously leaked raw SQLite errors when the source or target didn't exist (`storage error: FOREIGN KEY constraint failed`). Now inserts pre-flight `SELECT 1 FROM memory` checks for both ids; missing returns `memory not found: <id>`.

`call_identity_approve` / `call_identity_reject` previously returned `null` for already-resolved proposals, leaving the caller unable to distinguish "not found" from "already resolved". New `mnemush::identity::find_proposal(dir, id)` helper finds any-status proposal; the MCP layer translates `Ok(None)` into one of three messages: `proposal not found: <id>`, `proposal already approved`, `proposal already rejected`.

### Added

**`memory_neighbors` pi tool.** Was referenced in `memory_reflect`'s description ("call memory_neighbors to inspect") but not actually registered, forcing any caller to reach the graph out-of-process. Now: 10 pi tools (was 9). 1-tier BFS by default (`max_hops=2`), matching the spreading-activation config.

### Changed

**`memory` pi tool description corrected.** Said "add / search / replace / remove" but only `add` and `search` were implemented. Updated to "add or search" so the LLM doesn't expect `remove` and discover the gap at runtime.

**`after_tool_call` skip list now matches Pi tool names (not just OpenCode).** The 6/14-tool-call nudge counter previously skipped only OpenCode-style names (`mnemush-memory`, `mnemush-memory_search`, ...). In a pi session tool names have no `mnemush-` prefix, so calling our own `memory` or `memory_get` would still increment the counter and surface the nudge — wasted reminder. Skip list now matches both prefixes.

**Pi extension file-header tool list.** Updated to enumerate all 10 tools.

13 new unit tests in `bin/mnemush-mcp`: `range_error` × 4 (out-of-range, negative, NaN, boundaries); identity × 4 (unknown id, already approved, already rejected, approve-after-reject); unknown-value × 4 (unknown category, unknown memory_type, unknown source_id, unknown target_id); link-test × 1.

Total: 82 unit tests pass (69 lib + 13 bin).

## v0.2.0 — 2026-07-01

v0.2 (auto-maintenance) and v0.3 first cut (graph intelligence) squashed into a single release commit. Headline: mnemush now runs without user intervention, the LLM can self-curate via new MCP tools, and search uses 1-hop graph expansion to surface related memories.

_Note: the items below were originally filed under `## Unreleased` in the prior commit, but actually shipped as part of v0.2.0. They have been moved here per `docs/RELEASING.md`. The release itself was not yet published to crates.io at the time of this audit, so the v0.2.0 cut date remains 2026-07-01._

### Fixed

**Enum parse errors surfaced as a misleading `Conversion error from type Text at index: 0` wrapper.** A user's DB had rows with `tier='active'` (not in the v0.2 `Tier` enum). The `impl From<MnemushError> for rusqlite::Error` produced `FromSqlConversionFailure(0, Type::Text, ...)` whose Display buries the real MnemushError. Changed to `ToSqlConversionFailure(Box::new(e))` — Display now shows the actual `unknown Tier: 'active'` (or similar) at the top level. Affected `row_to_memory` closure path (auto-link layer A, reflect candidates, mnemush_status, MCP reads). 1 new unit test (`unknown_tier_errors_without_misleading_wrapper`); updated existing test (`unknown_category_errors_instead_of_silent_fallback`) to assert the misleading wrapper is absent. Documented as `D13` in `docs/decisions.md`.

**Active forgetting was declared but never invoked.** `should_prune` and the configured thresholds existed since v0.1 but no code path called them. Now wired through `mnemush prune` (and the session_end hook) so the thresholds actually take effect.

**`sanitize_fts_query` joined tokens with a single space**, which FTS5 interprets as a phrase query (all terms in sequence) — meaning any multi-word query found nothing in practice. Changed to `OR` separator so the default semantics are "any term matches". This affected `memory_search` and the auto-link conflict detector, both of which now return more candidates (a feature, not a regression).

**`MNEMUSH_DATA_DIR` env var was honored only by `identity::default_identity_dir` and `forget::prune_*` (via `Store::open`).** `mnemush init` and the config's `db_path` ignored it, making `MNEMUSH_DATA_DIR=/tmp/foo mnemush init` write to the real `~/.mnemush/identity/`. Now `init_dotfiles` and `apply_env_overrides` both honor it. `MNEMUSH_DB_PATH` still wins if set explicitly.

**`identity approve` / `reject` required the full 36-char UUID.** Now any prefix ≥ 4 chars that matches a pending proposal works. `list-pending` output also prints the full `id:` line so users can copy-paste it.

### Added

**Edge decay.** `EdgeConfig.edge_decay_half_life_days` (default 60d) was declared in v0.1 but never applied. New `forget::current_edge_strength` and `decay_all_edges` implement the same Ebbinghaus formula used for memory confidence. Wired into the pi extension's `session_end` hook (default ON; `MNEMUSH_EDGE_DECAY_ON_SESSION_END=off` to skip). Without this pass, the memory graph accumulated noise as edges were never weakened. New CLI: `mnemush edge-decay`. 8 new unit tests.

**`process_needs_review` queue handler.** The `needs_review` flag was set by the v0.2 tool-error capture (`after_tool_call`) but never cleared. New `forget::process_needs_review(store, grace)` clears the flag on items older than the grace period and downgrades importance by 0.1 per pass on `category=failure` items (so repeated errors fade naturally). Wired into `session_end` (default ON; `MNEMUSH_NEEDS_REVIEW_ON_SESSION_END=off` to skip; `MNEMUSH_NEEDS_REVIEW_GRACE_DAYS=N` to adjust). New CLI: `mnemush process-needs-review [--grace-days N]`. 4 new unit tests.

**Identity reflection.** The LLM can propose updates to `USER.md` / `PERSONA.md` / `CONSTITUTION.md` via the `identity_propose` MCP / CLI / pi tool, but updates are never applied silently. Proposals are written to `~/.mnemush/identity/pending.jsonl` with id, target, content, reason, evidence_count, and status (`pending` | `approved` | `rejected`). The user reviews via `mnemush identity list-pending` and applies with `approve` / `reject`. CLI subcommands: `mnemush identity show|list-pending|propose|approve|reject`. MCP tools: `identity_propose`, `identity_list_pending`, `identity_approve`, `identity_reject`. Pi tools: `identity_propose`, `identity_review`. 8 new unit tests.

**`memory_save_search_result` tool.** Explicit (not auto) save of search hits as memories. Takes `ids` (from a prior `memory_search`) and `query` (recorded in context for provenance). Returns `{saved: [...ids], errors: [...]}` so the caller knows which inputs succeeded. Empty `ids` or missing `query` returns a proper -32602 error.

**Insight / eureka mechanism in two layers.** Layer A (algorithmic, on every `memory_add`): `auto_link_tx` step 3 runs a separate FTS5 OR-query against recent content, computes Jaccard similarity, and adds up to 3 low-strength `related` edges for memories in the `[0.05, 0.5)` similarity band. Configurable via new `edges.auto_link_weak_*` fields. Skips pairs already linked. Layer B (LLM-driven, on demand): `MemoryApi::reflect_candidates(now, since_days, limit)` returns recent least-connected memories. CLI: `mnemush reflect [--since-days N] [--limit N]`. MCP: `memory_reflect`. Pi: `memory_reflect`. 6 new unit tests.

**`mnemush status` subcommand + `mnemush_status` MCP tool.** One-line summary of memory system state: active/soft-deleted counts, edge count, needs_review count, prune candidates (using `should_prune`), reflect candidates (last 7d), pending identity proposals.

**Spreading activation on search.** `memory_search` expands each top hit with its 1-hop neighbors, scoring them at `hit.score * edge.strength * 0.5`. Lets the LLM find related memories that didn't match the query text directly. Gated on `edges.max_neighbor_hops`; set to 0 to disable without code changes. 2 new unit tests.

**Periodic insight-save nudge (mnemush-pi).** On every 6th and 14th non-mnemush tool call in a turn, the pi extension surfaces a `sendStatus` reminder. Counter resets on `before_agent_start`. The existing error-capture handler skips ALL `mnemush-*` tools so our own failures aren't recorded as "tool failure" memories.

**4 new config fields in `[edges]`.** `auto_link_weak_min_sim` (0.05), `auto_link_weak_max_sim` (0.5), `auto_link_weak_strength` (0.4), `auto_link_weak_limit` (3). See `docs/config.example.toml`.

### Changed

`docs/identity/PERSONA.md`: appended an agent-centric memory behavior section (preference/decision/correction capture patterns, what NOT to save). Bootstrapped automatically by `identity_propose`+`identity_approve`.

### Test count

56/56 unit tests pass.

### Known limitations (v0.2)

- Orphan FTS5 rows after hard-delete (`--isolate` removes the memory row without rebuilding `memory_fts`; a `mnemush vacuum` would be a future addition).
- `mnemush prune --isolate` (hard delete) is never auto-invoked; users opt in manually.

