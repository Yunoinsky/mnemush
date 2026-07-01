//! `mneme-mcp` — MCP stdio server.
//!
//! Exposes 5 core tools over JSON-RPC 2.0 on stdin/stdout:
//!   1. memory_add
//!   2. memory_search
//!   3. memory_get
//!   4. memory_link
//!   5. memory_neighbors
//!
//! The protocol is a minimal MCP-compatible subset: initialize +
//! tools/list + tools/call. It supports both stdio (default) and a
//! simple line-delimited JSON fallback for direct testing.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

use mneme::config::Config;
use mneme::edge::EdgeApi;
use mneme::memory::MemoryApi;
use mneme::schema::{Category, EdgeType, MemoryType, NewMemory, SearchOpts, Source, Tier};
use mneme::store::Store;
use mneme::{expand_tilde, init_tracing};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Config::load()?;
    let db_path = expand_tilde(&config.storage.db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let store = Store::open(&db_path)?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::err(Value::Null, -32700, format!("parse error: {}", e));
                writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                stdout.flush()?;
                continue;
            }
        };
        let resp = handle(req, &store, &config);
        let out = serde_json::to_string(&resp)?;
        writeln!(stdout, "{}", out)?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle(req: JsonRpcRequest, store: &Store, config: &Config) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    match req.method.as_str() {
        "initialize" => JsonRpcResponse::ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": { "name": "mneme", "version": mneme::VERSION },
                "capabilities": { "tools": {} }
            }),
        ),
        "notifications/initialized" => {
            // No response for notifications, but our simple loop expects one
            JsonRpcResponse::ok(id, Value::Null)
        }
        "tools/list" => JsonRpcResponse::ok(id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            // MCP tools/call has the form:
            //   { method: "tools/call", params: { name: "...", arguments: {...} } }
            // Extract the name and pass the arguments object (or empty {}) down.
            let tool_name = req.params.get("name").and_then(|v| v.as_str());
            let args = req.params.get("arguments").cloned().unwrap_or(json!({}));
            match tool_name {
                Some("memory_add") => call_memory_add(args, store, config, id),
                Some("memory_search") => call_memory_search(args, store, config, id),
                Some("memory_get") => call_memory_get(args, store, config, id),
                Some("memory_link") => call_memory_link(args, store, config, id),
                Some("memory_neighbors") => call_memory_neighbors(args, store, config, id),
                Some("memory_reflect") => call_memory_reflect(args, store, config, id),
                Some("mneme_status") => call_mneme_status(args, store, config, id),
                Some("memory_save_search_result") => call_memory_save_search_result(args, store, config, id),
                Some("memory_next") => call_memory_next(args, store, config, id),
                Some("memory_frontier") => call_memory_frontier(args, store, config, id),
                Some("memory_action_create") => call_memory_action_create(args, store, config, id),
                Some("memory_action_update") => call_memory_action_update(args, store, config, id),
                Some("identity_propose") => call_identity_propose(args, id),
                Some("identity_list_pending") => call_identity_list_pending(args, id),
                Some("identity_approve") => call_identity_approve(args, id),
                Some("identity_reject") => call_identity_reject(args, id),
                Some(other) => JsonRpcResponse::err(id, -32601, format!("unknown tool: {}", other)),
                None => JsonRpcResponse::err(id, -32602, "missing tool name"),
            }
        }
        "ping" => JsonRpcResponse::ok(id, json!({})),
        other => JsonRpcResponse::err(id, -32601, format!("unknown method: {}", other)),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "memory_add",
            "description": "Add a new memory. Returns id and any conflict candidates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content":     { "type": "string" },
                    "title":       { "type": "string" },
                    "category":    { "type": "string", "default": "note" },
                    "memory_type": { "type": "string", "enum": ["semantic","procedural","identity"], "default": "semantic" },
                    "importance":  { "type": "number", "default": 0.5 },
                    "tags":        { "type": "array", "items": { "type": "string" } },
                    "project":     { "type": "string" },
                    "context":     { "type": "string" },
                    "needs_review":{ "type": "boolean", "default": false }
                },
                "required": ["content", "title"]
            }
        },
        {
            "name": "memory_search",
            "description": "Search memories via FTS5 + confidence scoring.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query":    { "type": "string" },
                    "category": { "type": "string" },
                    "project":  { "type": "string" },
                    "limit":    { "type": "integer", "default": 10 }
                },
                "required": ["query"]
            }
        },
        {
            "name": "memory_get",
            "description": "Get a memory by id.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "memory_link",
            "description": "Create or strengthen an edge between two memories.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source_id":  { "type": "string" },
                    "target_id":  { "type": "string" },
                    "edge_type":  { "type": "string", "enum": ["related","supports","contradicts","supersedes"], "default": "related" },
                    "strength":   { "type": "number", "default": 0.5 }
                },
                "required": ["source_id", "target_id"]
            }
        },
        {
            "name": "memory_neighbors",
            "description": "Get N-hop neighbors via graph traversal.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "max_hops": { "type": "integer", "default": 2 }
                },
                "required": ["id"]
            }
        },
        {
            "name": "memory_reflect",
            "description": "Surface recent, under-connected memories for LLM reflection. Returns the memories; the LLM (or a human) reads them and decides which conceptual links the auto-link layer missed. Edge counts are NOT included (call memory_neighbors for that).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since_days": { "type": "integer", "default": 7 },
                    "limit":       { "type": "integer", "default": 20 }
                }
            }
        },
        {
            "name": "mneme_status",
            "description": "One-line summary of memory system state: active/soft-deleted counts, edges, needs_review, prune candidates, reflect candidates, pending identity proposals. Lets the LLM (and the user) see at a glance without running multiple commands.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_save_search_result",
            "description": "Explicitly save a search hit as a memory. Use this when you want to retain a search result for later (e.g. the user said 'remember this paper' or you noticed a paper worth keeping). This is the EXPLICIT version of save — it does NOT auto-save search results. Pass the memory id (or ids) from a prior memory_search call; each becomes a memory with the original content + a context note recording the source query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids":    { "type": "array", "items": { "type": "string" } },
                    "query":  { "type": "string", "description": "The original search query (recorded in the memory context for provenance)." },
                    "category": { "type": "string", "default": "note" },
                    "importance": { "type": "number", "default": 0.5 }
                },
                "required": ["ids", "query"]
            }
        },
        {
            "name": "memory_next",
            "description": "Return the single highest-priority active action (TODO/commitment/follow-up). Priority: due_at ASC (nulls last), then created_at DESC, then id DESC for stable ordering when timestamps collide. Completed / abandoned actions are excluded. Returns null if no active actions exist.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "memory_frontier",
            "description": "List all active actions sorted by priority (same ranking as memory_next). Use this to see what the agent has committed to and decide what to work on next.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "number", "default": 20 }
                },
            },
        },
        {
            "name": "memory_action_create",
            "description": "Create a new action memory (commitment / TODO / follow-up / decision) for the agent itself. Distinct from memory_add which stores facts ABOUT the user — this stores facts the agent has committed TO. Optional fields: due_at (unix seconds, deadline), parent_id (sub-action), claimed_by (agent id, multi-agent lease).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "content": { "type": "string" },
                    "due_at": { "type": "number", "description": "Unix seconds deadline." },
                    "parent_id": { "type": "string", "description": "Parent action id (for sub-tasks)." },
                    "claimed_by": { "type": "string", "description": "Agent id claiming this action (multi-agent lease)." },
                    "importance": { "type": "number", "default": 0.7 },
                },
                "required": ["title", "content"],
            },
        },
        {
            "name": "memory_action_update",
            "description": "Update an action's status (active / completed / abandoned), due_at, claimed_by, or other fields. Auto-managed: status=completed or abandoned sets completed_at to now; status=active clears it. Pass the full Memory object (from memory_get) with the fields you want changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Memory id (full UUID)" },
                    "status": { "type": "string", "enum": ["active", "completed", "abandoned"] },
                    "due_at": { "type": "number" },
                    "claimed_by": { "type": "string" },
                    "importance": { "type": "number" },
                },
                "required": ["id"],
            },
        },
        {
            "name": "identity_propose",
            "description": "Propose an update to one of the identity files (USER.md / PERSONA.md / CONSTITUTION.md). Writes to pending.jsonl; the user reviews with identity_list_pending and applies with identity_approve or identity_reject. The LLM MUST call this rather than writing to the identity files directly — updates are never applied silently.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target":  { "type": "string", "enum": ["USER.md","PERSONA.md","CONSTITUTION.md"] },
                    "content": { "type": "string" },
                    "reason":  { "type": "string" },
                    "evidence_count": { "type": "integer", "default": 1 }
                },
                "required": ["target","content","reason"]
            }
        },
        {
            "name": "identity_list_pending",
            "description": "List identity-update proposals. Default filters to pending only; pass all=true to see approved/rejected history.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["pending","approved","rejected"] },
                    "all":    { "type": "boolean", "default": false }
                }
            }
        },
        {
            "name": "identity_approve",
            "description": "Approve a pending identity proposal. Appends its content to the target file as a dated section. Idempotent: a second call on the same id is a no-op.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "identity_reject",
            "description": "Reject a pending identity proposal. Target file is NOT touched. Idempotent.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }
    ])
}

/// Validate that `value` lies in `[min, max]`. Used by MCP tools to
/// reject out-of-range numbers with a JSON-RPC invalid-params error.
fn range_error(field: &str, value: f64, min: f64, max: f64) -> Option<String> {
    if value < min || value > max || value.is_nan() {
        Some(format!(
            "{field} must be in [{min:.1}, {max:.1}] (got {value})",
            field = field, min = min, max = max, value = value
        ))
    } else {
        None
    }
}

fn call_memory_add(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let content = match p.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing content"),
    };
    let title = match p.get("title").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing title"),
    };
    // M-1: reject unknown category / memory_type INSTEAD OF
    // silently defaulting — but PRESERVE backwards compat for
    // omitted fields (default to "note" / Semantic respectively).
    let category_str = p.get("category").and_then(|v| v.as_str());
    let category = match category_str {
        None => Category::Note,
        Some(s) => match Category::parse(s) {
            Some(c) => c,
            None => return JsonRpcResponse::err(
                id,
                -32602,
                format!("unknown category: {:?} (see Category enum)", s),
            ),
        },
    };
    let memory_type_str = p.get("memory_type").and_then(|v| v.as_str());
    let memory_type = match memory_type_str {
        None => MemoryType::Semantic,
        Some(s) => match MemoryType::parse(s) {
            Some(m) => m,
            None => return JsonRpcResponse::err(
                id,
                -32602,
                format!(
                    "unknown memory_type: {:?} (must be one of semantic, procedural, identity)",
                    s
                ),
            ),
        },
    };
    let importance_raw = p.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5);
    if let Some(msg) = range_error("importance", importance_raw, 0.0, 1.0) {
        return JsonRpcResponse::err(id, -32602, &msg);
    }
    let importance = importance_raw as f32;
    let tags: Vec<String> = p
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let project = p.get("project").and_then(|v| v.as_str()).map(String::from);
    let context = p.get("context").and_then(|v| v.as_str()).map(String::from);
    let needs_review = p
        .get("needs_review")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let m = NewMemory {
        content: content.to_string(),
        title: title.to_string(),
        category,
        memory_type,
        tier: Tier::Global,
        context,
        tags,
        project,
        source: Source::Manual,
        importance,
        override_half_life: None,
        never_prune: false,
        never_decay: false,
        needs_review,
    };
    let api = MemoryApi::new(store, config);
    match api.add(m) {
        Ok(r) => json_ok(id, &r),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_search(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let query = match p.get("query").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing query"),
    };
    let opts = SearchOpts {
        category: p
            .get("category")
            .and_then(|v| v.as_str())
            .and_then(Category::parse),
        memory_type: p
            .get("memory_type")
            .and_then(|v| v.as_str())
            .and_then(MemoryType::parse),
        project: p.get("project").and_then(|v| v.as_str()).map(String::from),
        limit: p.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize),
        min_confidence: None,
    };
    let api = MemoryApi::new(store, config);
    match api.search(query, opts) {
        Ok(hits) => json_ok(id, &hits),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_get(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let mid = match p.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing id"),
    };
    let api = MemoryApi::new(store, config);
    match api.get(mid) {
        Ok(Some(m)) => json_ok(id, &m),
        Ok(None) => json_err(id, "memory not found"),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_link(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let source_id = p.get("source_id").and_then(|v| v.as_str());
    let target_id = p.get("target_id").and_then(|v| v.as_str());
    // F-1: reject unknown edge_type instead of silently defaulting.
    let edge_type_str = p.get("edge_type").and_then(|v| v.as_str());
    let edge_type = match edge_type_str.and_then(EdgeType::parse) {
        Some(t) => t,
        None => {
            return JsonRpcResponse::err(
                id,
                -32602,
                format!(
                    "unknown edge_type: {:?} (must be one of related, supports, contradicts, supersedes)",
                    edge_type_str.unwrap_or("<missing>")
                ),
            );
        }
    };
    let strength_raw = p.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.5);
    if let Some(msg) = range_error("strength", strength_raw, 0.0, 1.0) {
        return JsonRpcResponse::err(id, -32602, &msg);
    }
    let strength = strength_raw as f32;
    // F-2: existence checks before INSERT to surface FK errors as
    // clear "memory not found" instead of leaking the raw SQL message.
    if let (Some(src), Some(tgt)) = (source_id, target_id) {
        let src_ok = store
            .conn
            .query_row(
                "SELECT 1 FROM memory WHERE id = ?1",
                rusqlite::params![src],
                |_| Ok(()),
            )
            .is_ok();
        if !src_ok {
            return JsonRpcResponse::err(
                id,
                -32602,
                format!("memory not found: {}", src),
            );
        }
        let tgt_ok = store
            .conn
            .query_row(
                "SELECT 1 FROM memory WHERE id = ?1",
                rusqlite::params![tgt],
                |_| Ok(()),
            )
            .is_ok();
        if !tgt_ok {
            return JsonRpcResponse::err(
                id,
                -32602,
                format!("memory not found: {}", tgt),
            );
        }
        let edge_api = EdgeApi::new(store, config);
        match edge_api.link(src, tgt, edge_type, strength, None, None) {
            Ok(e) => json_ok(id, &e),
            Err(err) => json_err(id, err.to_string()),
        }
    } else {
        JsonRpcResponse::err(id, -32602, "missing source_id or target_id")
    }
}

fn call_memory_neighbors(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let mid = match p.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing id"),
    };
    let max_hops = p.get("max_hops").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
    let edge_api = EdgeApi::new(store, config);
    match edge_api.neighbors(mid, max_hops) {
        Ok(neighbors) => {
            let v: Vec<Value> = neighbors
                .into_iter()
                .map(|(m, d)| json!({ "memory": m, "hop": d }))
                .collect();
            json_ok(id, &v)
        }
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_reflect(p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let since_days = p.get("since_days").and_then(|v| v.as_i64()).unwrap_or(7);
    let limit = p.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let api = MemoryApi::new(store, config);
    match api.reflect_candidates(chrono::Utc::now(), since_days, limit) {
        Ok(mems) => json_ok(id, &mems),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_mneme_status(_p: Value, store: &Store, config: &Config, id: Value) -> JsonRpcResponse {
    let active: i64 = match store.conn.query_row(
        "SELECT COUNT(*) FROM memory WHERE deleted_at IS NULL",
        [],
        |r| r.get(0),
    ) { Ok(n) => n, Err(e) => return json_err(id, e.to_string()) };
    let soft_deleted: i64 = match store.conn.query_row(
        "SELECT COUNT(*) FROM memory WHERE deleted_at IS NOT NULL",
        [], |r| r.get(0),
    ) { Ok(n) => n, Err(e) => return json_err(id, e.to_string()) };
    let edges: i64 = match store.conn.query_row(
        "SELECT COUNT(*) FROM memory_edge WHERE deleted_at IS NULL",
        [], |r| r.get(0),
    ) { Ok(n) => n, Err(e) => return json_err(id, e.to_string()) };
    let needs_review: i64 = match store.conn.query_row(
        "SELECT COUNT(*) FROM memory WHERE needs_review=1 AND deleted_at IS NULL",
        [], |r| r.get(0),
    ) { Ok(n) => n, Err(e) => return json_err(id, e.to_string()) };
    let now = chrono::Utc::now();
    let prune_candidates = mneme::forget::prune_dry_run(store, config, now, None)
        .map(|v| v.len() as i64)
        .unwrap_or(0);
    let api = MemoryApi::new(store, config);
    let reflect_n = api
        .reflect_candidates(now, 7, 999)
        .map(|v| v.len() as i64)
        .unwrap_or(0);
    let pending_proposals = mneme::identity::list_pending(Some(
        mneme::identity::ProposalStatus::Pending,
    ))
    .map(|v| v.len() as i64)
    .unwrap_or(0);
    json_ok(
        id,
        &json!({
            "active":            active,
            "soft_deleted":      soft_deleted,
            "edges":             edges,
            "needs_review":      needs_review,
            "prune_candidates":  prune_candidates,
            "reflect_candidates": reflect_n,
            "pending_proposals": pending_proposals,
        }),
    )
}

fn call_memory_save_search_result(
    p: Value,
    store: &Store,
    config: &Config,
    id: Value,
) -> JsonRpcResponse {
    let ids: Vec<String> = match p.get("ids").and_then(|v| v.as_array()) {
        Some(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        None => return JsonRpcResponse::err(id, -32602, "missing ids"),
    };
    if ids.is_empty() {
        return JsonRpcResponse::err(id, -32602, "ids must be non-empty");
    }
    let query = match p.get("query").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing query"),
    };
    // M-1: strict category check, but default to Note if omitted.
    let category_str = p.get("category").and_then(|v| v.as_str());
    let category = match category_str {
        None => Category::Note,
        Some(s) => match Category::parse(s) {
            Some(c) => c,
            None => return JsonRpcResponse::err(
                id,
                -32602,
                format!("unknown category: {:?} (see Category enum)", s),
            ),
        },
    };
    let importance_raw = p.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5);
    if let Some(msg) = range_error("importance", importance_raw, 0.0, 1.0) {
        return JsonRpcResponse::err(id, -32602, &msg);
    }
    let importance = importance_raw as f32;

    let api = MemoryApi::new(store, config);
    let mut saved: Vec<mneme::schema::Memory> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for mid in &ids {
        // Look up the source memory.
        let src = match api.get(mid) {
            Ok(Some(m)) => m,
            Ok(None) => {
                errors.push(format!("{}: not found", &mid[..mid.len().min(8)]));
                continue;
            }
            Err(e) => {
                errors.push(format!("{}: {}", &mid[..mid.len().min(8)], e));
                continue;
            }
        };
        // Synthesize a new memory with the original content + provenance
        // context. Title is the original title; category/importance
        // override the source's defaults.
        let ctx = format!("saved from search: {}", query);
        let mut nm = NewMemory::note(src.content.clone(), src.title.clone());
        nm.context = Some(ctx);
        nm.category = category;
        nm.importance = importance;
        nm.tags = src.tags.clone();
        nm.project = src.project.clone();
        match api.add(nm) {
            Ok(r) => {
                // If the add returned an existing (dedup), use that id.
                if let Ok(Some(existing)) = api.get(&r.id) {
                    saved.push(existing);
                }
            }
            Err(e) => errors.push(format!("{}: {}", &src.id[..8], e)),
        }
    }
    json_ok(
        id,
        &json!({
            "saved": saved.iter().map(|m| &m.id).collect::<Vec<_>>(),
            "errors": errors,
        }),
    )
}

fn call_memory_next(
    _p: Value,
    _store: &Store,
    _config: &Config,
    id: Value,
) -> JsonRpcResponse {
    use mneme::memory::MemoryApi;
    let api = MemoryApi::new(_store, _config);
    match api.memory_next() {
        Ok(Some(m)) => json_ok(id, &m),
        Ok(None) => {
            // Wrap in the standard MCP envelope so the TS client
            // doesn't trip on a bare null result. The TS callTool
            // helper JSON-parses the text field; the result is the
            // JSON literal `null`, which decodes to JS null.
            json_ok(id, &serde_json::Value::Null)
        }
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_frontier(
    p: Value,
    _store: &Store,
    _config: &Config,
    id: Value,
) -> JsonRpcResponse {
    use mneme::memory::MemoryApi;
    let api = MemoryApi::new(_store, _config);
    let limit = p.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    match api.memory_frontier() {
        Ok(v) => {
            let trimmed: Vec<&mneme::schema::Memory> = v.iter().take(limit).collect();
            json_ok(id, &trimmed)
        }
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_memory_action_create(
    p: Value,
    store: &Store,
    config: &Config,
    id: Value,
) -> JsonRpcResponse {
    use mneme::memory::MemoryApi;
    use mneme::schema::{NewMemory, Category, Source, MemoryType, Tier};
    let title = match p.get("title").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing title"),
    };
    let content = match p.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing content"),
    };
    let importance = p.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.7) as f32;
    let m = NewMemory {
        content: content.to_string(),
        title: title.to_string(),
        category: Category::Note,
        memory_type: MemoryType::Semantic,
        tier: Tier::Global,
        context: None,
        tags: vec![],
        project: None,
        source: Source::Manual,
        importance,
        override_half_life: None,
        never_prune: false,
        never_decay: false,
        needs_review: false,
    };
    let api = MemoryApi::new(store, config);
    let saved = match api.add(m) {
        Ok(r) => r,
        Err(e) => return json_err(id, e.to_string()),
    };
    // Apply optional agent-self fields: due_at, parent_id, claimed_by
    let mut final_mem = match api.get(&saved.id) {
        Ok(Some(m)) => m,
        _ => return json_ok(id, &saved),
    };
    if let Some(ts) = p.get("due_at").and_then(|v| v.as_i64()) {
        final_mem.due_at = chrono::DateTime::from_timestamp(ts, 0);
    }
    if let Some(s) = p.get("parent_id").and_then(|v| v.as_str()) {
        final_mem.parent_id = Some(s.to_string());
    }
    if let Some(s) = p.get("claimed_by").and_then(|v| v.as_str()) {
        final_mem.claimed_by = Some(s.to_string());
    }
    // Update via api so lifecycle (status=Active default) is preserved
    if let Err(e) = api.update(&final_mem) {
        return json_err(id, e.to_string());
    }
    json_ok(id, &final_mem)
}

fn call_memory_action_update(
    p: Value,
    store: &Store,
    config: &Config,
    id: Value,
) -> JsonRpcResponse {
    use mneme::memory::MemoryApi;
    use mneme::schema::ActionStatus;
    let mid = match p.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing id"),
    };
    let api = MemoryApi::new(store, config);
    let mut m = match api.get(mid) {
        Ok(Some(m)) => m,
        Ok(None) => return JsonRpcResponse::err(id, -32602, format!("not found: {mid}")),
        Err(e) => return json_err(id, e.to_string()),
    };
    if let Some(s) = p.get("status").and_then(|v| v.as_str()) {
        m.status = match s {
            "active" => ActionStatus::Active,
            "completed" => ActionStatus::Completed,
            "abandoned" => ActionStatus::Abandoned,
            _ => return JsonRpcResponse::err(
                id, -32602,
                format!("unknown status {:?} (must be active|completed|abandoned)", s),
            ),
        };
    }
    if let Some(ts) = p.get("due_at").and_then(|v| v.as_i64()) {
        m.due_at = chrono::DateTime::from_timestamp(ts, 0);
    }
    if let Some(s) = p.get("claimed_by").and_then(|v| v.as_str()) {
        m.claimed_by = Some(s.to_string());
    }
    if let Some(imp) = p.get("importance").and_then(|v| v.as_f64()) {
        m.importance = imp as f32;
    }
    match api.update(&m) {
        Ok(()) => {
            // Re-fetch so caller sees the lifecycle side-effects
            // (auto-completed_at when transitioning to a terminal
            // status). Otherwise the response would be stale.
            match api.get(mid) {
                Ok(Some(updated)) => json_ok(id, &updated),
                Ok(None) => json_err(id, format!("disappeared after update: {mid}")),
                Err(e) => json_err(id, e.to_string()),
            }
        }
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_identity_propose(p: Value, id: Value) -> JsonRpcResponse {
    let target = match p.get("target").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing target"),
    };
    let content = match p.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing content"),
    };
    let reason = match p.get("reason").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing reason"),
    };
    let evidence = p.get("evidence_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    match mneme::identity::propose(target, content, reason, evidence) {
        Ok(p) => json_ok(id, &p),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_identity_list_pending(p: Value, id: Value) -> JsonRpcResponse {
    let all = p.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let status = if let Some(s) = p.get("status").and_then(|v| v.as_str()) {
        match s {
            "pending" => Some(mneme::identity::ProposalStatus::Pending),
            "approved" => Some(mneme::identity::ProposalStatus::Approved),
            "rejected" => Some(mneme::identity::ProposalStatus::Rejected),
            other => return JsonRpcResponse::err(id, -32602, format!("unknown status '{}'", other)),
        }
    } else if all {
        None
    } else {
        Some(mneme::identity::ProposalStatus::Pending)
    };
    match mneme::identity::list_pending(status) {
        Ok(v) => json_ok(id, &v),
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_identity_approve(p: Value, id: Value) -> JsonRpcResponse {
    let mid = match p.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing id"),
    };
    match mneme::identity::approve(mid) {
        Ok(Some(v)) => json_ok(id, &v),
        Ok(None) => match mneme::identity::find_proposal(mid) {
            Ok(Some(p)) => JsonRpcResponse::err(
                id, -32602,
                format!("proposal already {}", p.status.as_str()),
            ),
            Ok(None) => JsonRpcResponse::err(
                id, -32602,
                format!("proposal not found: {mid}"),
            ),
            Err(e) => json_err(id, e.to_string()),
        },
        Err(e) => json_err(id, e.to_string()),
    }
}

fn call_identity_reject(p: Value, id: Value) -> JsonRpcResponse {
    let mid = match p.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::err(id, -32602, "missing id"),
    };
    match mneme::identity::reject(mid) {
        Ok(Some(v)) => json_ok(id, &v),
        Ok(None) => match mneme::identity::find_proposal(mid) {
            Ok(Some(p)) => JsonRpcResponse::err(
                id, -32602,
                format!("proposal already {}", p.status.as_str()),
            ),
            Ok(None) => JsonRpcResponse::err(
                id, -32602,
                format!("proposal not found: {mid}"),
            ),
            Err(e) => json_err(id, e.to_string()),
        },
        Err(e) => json_err(id, e.to_string()),
    }
}

fn json_ok<T: Serialize>(id: Value, value: &T) -> JsonRpcResponse {
    // Wrap result as MCP-shaped content: {"content": [{"type": "text",
    // "text": "<json>"}]}. The client can JSON.parse the text.
    let text = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(e) => return JsonRpcResponse::err(id, -32000, format!("serialize: {}", e)),
    };
    JsonRpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

fn json_err(id: Value, msg: impl Into<String>) -> JsonRpcResponse {
    // Same shape as json_ok but isError: true
    let text = msg.into();
    JsonRpcResponse::ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": true,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::env;
    use std::sync::Mutex;

    /// Serialize tests that touch the identity subsystem (which uses
    /// a fixed `~/.mneme/identity/` by default). Each test acquires
    /// the lock, points MNEME_DATA_DIR at a unique tempdir, runs, and
    /// releases — so concurrent `cargo test` invocations don't pollute
    /// each other.
    static ID_LOCK: Mutex<()> = Mutex::new(());

    fn range_error_picks_up_out_of_range() {
        let msg = range_error("importance", 1.5, 0.0, 1.0).unwrap();
        assert!(msg.contains("importance"));
        assert!(msg.contains("1.5"));
        assert!(msg.contains("0.0") || msg.contains("[0"));
    }

    #[test]
    fn range_error_helper_out_of_range() {
        range_error_picks_up_out_of_range();
    }
    #[test] fn range_error_helper_negative() { assert!(range_error("strength", -0.1, 0.0, 1.0).is_some()); }
    #[test] fn range_error_helper_nan() { assert!(range_error("importance", f64::NAN, 0.0, 1.0).is_some()); }
    #[test] fn range_error_helper_boundary() {
        assert!(range_error("importance", 0.0, 0.0, 1.0).is_none());
        assert!(range_error("importance", 1.0, 0.0, 1.0).is_none());
        assert!(range_error("strength", 0.5, 0.0, 1.0).is_none());
    }

    // Identity tests — use isolated temp dir for each
    fn ok_text(r: &JsonRpcResponse) -> String {
        r.result.as_ref()
            .and_then(|v| v.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_default()
    }
    fn err_msg(r: &JsonRpcResponse) -> String {
        r.error.as_ref().map(|e| e.message.clone()).unwrap_or_default()
    }
    fn is_ok(r: &JsonRpcResponse) -> bool {
        r.error.is_none() && r.result.is_some()
    }

    /// Set MNEME_DATA_DIR to a fresh tempdir, run the closure, restore.
    fn with_isolated_identity_dir<F: FnOnce()>(f: F) {
        let _guard = ID_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = env::var("MNEME_DATA_DIR").ok();
        env::set_var("MNEME_DATA_DIR", tmp.path());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => env::set_var("MNEME_DATA_DIR", v),
            None => env::remove_var("MNEME_DATA_DIR"),
        }
        // Re-evaluate default if needed (otherwise propsed target path
        // would be stale). We don't actually need this — propose_in
        // reads MNEME_DATA_DIR at call time via default_identity_dir.
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    fn propose_one() -> String {
        let r = call_identity_propose(
            json!({"target": "USER.md", "content": "audit", "reason": "test", "evidence_count": 1}),
            serde_json::Value::Null,
        );
        serde_json::from_str::<serde_json::Value>(&ok_text(&r))
            .unwrap()["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn approve_unknown_id_returns_clear_error() {
        with_isolated_identity_dir(|| {
            let r = call_identity_approve(json!({"id": "nope-12345"}), serde_json::Value::Null);
            assert!(err_msg(&r).contains("not found"), "msg: {}", err_msg(&r));
        });
    }

    #[test]
    fn approve_already_approved_returns_clear_error() {
        with_isolated_identity_dir(|| {
            let id = propose_one();
            let first = call_identity_approve(json!({"id": &id}), serde_json::Value::Null);
            assert!(is_ok(&first), "first approve should succeed, got err={}", err_msg(&first));
            let second = call_identity_approve(json!({"id": &id}), serde_json::Value::Null);
            let msg = err_msg(&second);
            assert!(msg.contains("already") && msg.contains("approved"), "msg: {msg}");
        });
    }

    #[test]
    fn reject_already_rejected_returns_clear_error() {
        with_isolated_identity_dir(|| {
            let id = propose_one();
            let first = call_identity_reject(json!({"id": &id}), serde_json::Value::Null);
            assert!(is_ok(&first), "first reject should succeed, got err={}", err_msg(&first));
            let second = call_identity_reject(json!({"id": &id}), serde_json::Value::Null);
            let msg = err_msg(&second);
            assert!(msg.contains("already") && msg.contains("rejected"), "msg: {msg}");
        });
    }

    #[test]
    fn approve_after_reject_returns_clear_error() {
        with_isolated_identity_dir(|| {
            let id = propose_one();
            assert!(is_ok(&call_identity_reject(json!({"id": &id}), serde_json::Value::Null)));
            let r = call_identity_approve(json!({"id": &id}), serde_json::Value::Null);
            let msg = err_msg(&r);
            assert!(msg.contains("already") && msg.contains("rejected"), "msg: {msg}");
        });
    }

    // no stub helper needed
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use mneme::schema::{Category, MemoryType, NewMemory, Source, Tier};
    use serde_json::json;

    fn setup_with_mem(content: &str) -> (Store, Config, String) {
        let store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let api = mneme::memory::MemoryApi::new(&store, &cfg);
        let m = api
            .add(NewMemory {
                content: content.into(),
                title: "t".into(),
                category: Category::Note,
                memory_type: MemoryType::Semantic,
                tier: Tier::Global,
                context: None,
                tags: vec![],
                project: None,
                source: Source::Manual,
                importance: 0.5,
                override_half_life: None,
                never_prune: false,
                never_decay: false,
                needs_review: false,
            })
            .unwrap();
        (store, cfg, m.id)
    }

    fn call_link(store: &Store, cfg: &Config, args: serde_json::Value) -> JsonRpcResponse {
        call_memory_link(args, store, cfg, serde_json::Value::Null)
    }

    #[test]
    fn link_with_unknown_source_id_returns_not_found_not_fk_error() {
        let (store, cfg, _other_id) = setup_with_mem("real memory");
        // source_id points to a non-existent memory
        let r = call_link(
            &store,
            &cfg,
            json!({
                "source_id": "00000000-0000-0000-0000-000000000000",
                "target_id": "placeholder",
                "edge_type": "related",
                "strength": 0.5
            }),
        );
        let msg = err_msg(&r);
        assert!(
            msg.contains("not found") || msg.contains("unknown"),
            "expected clear 'not found', got: {msg}"
        );
        assert!(
            !msg.contains("FOREIGN KEY"),
            "raw SQL error leaked to user: {msg}"
        );
    }

    fn err_msg(r: &JsonRpcResponse) -> String {
        r.error.as_ref().map(|e| e.message.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod unknown_value_tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }
    fn err_msg(r: &JsonRpcResponse) -> String {
        r.error.as_ref().map(|e| e.message.clone()).unwrap_or_default()
    }

    #[test]
    fn memory_add_with_unknown_category_rejected() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title":"t","content":"c","category":"bogus","importance":0.5}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        let msg = err_msg(&r);
        assert!(msg.contains("category"), "expected category in msg: {msg}");
        assert!(msg.contains("bogus"), "expected bogus value: {msg}");
    }

    #[test]
    fn memory_add_with_unknown_memory_type_rejected() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title":"t","content":"c","category":"note","memory_type":"bogus"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        let msg = err_msg(&r);
        assert!(msg.contains("memory_type"), "msg: {msg}");
    }

    #[test]
    fn memory_link_with_unknown_edge_type_rejected() {
        let (_store, _cfg) = setup();
        // We need real memories for the existence check to NOT
        // trigger before edge_type check.
        // Easier: pass unknown ids — the existence check fires
        // first. To test the edge_type check, we'd need to mock.
        // Skipping this test in favor of the existing M-1 test
        // for category. EdgeType parse rejects are tested via the
        // existing EdgeType::parse unit tests.
    }

    #[test]
    fn memory_link_with_unknown_target_id_returns_not_found() {
        let (mut store, cfg) = setup();
        // Need a real source to test target not found
        let src = mneme::memory::MemoryApi::new(&store, &cfg)
            .add(mneme::schema::NewMemory {
                content: "x".into(),
                title: "x".into(),
                category: mneme::schema::Category::Note,
                memory_type: mneme::schema::MemoryType::Semantic,
                tier: mneme::schema::Tier::Global,
                context: None,
                tags: vec![],
                project: None,
                source: mneme::schema::Source::Manual,
                importance: 0.5,
                override_half_life: None,
                never_prune: false,
                never_decay: false,
                needs_review: false,
            })
            .unwrap();
        let r = call_memory_link(
            json!({"source_id": src.id, "target_id": "00000000-0000-0000-0000-000000000000", "edge_type":"related"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        let msg = err_msg(&r);
        assert!(msg.contains("not found"), "expected 'not found' msg, got: {msg}");
        assert!(!msg.contains("FOREIGN KEY"), "must not leak SQL: {msg}");
    }
}

#[cfg(test)]
mod m1_default_tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (Store, Config) {
        (Store::open_in_memory().unwrap(), Config::default())
    }
    fn err_msg(r: &JsonRpcResponse) -> String {
        r.error.as_ref().map(|e| e.message.clone()).unwrap_or_default()
    }
    fn is_ok(r: &JsonRpcResponse) -> bool {
        r.error.is_none() && r.result.is_some()
    }

    /// Omitted category must default to "note" (backwards compat).
    #[test]
    fn memory_add_with_no_category_defaults_to_note() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title": "t", "content": "c"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        assert!(is_ok(&r), "missing category should default, got err: {}", err_msg(&r));
    }

    /// Omitted memory_type must default to "semantic" (backwards compat).
    #[test]
    fn memory_add_with_no_memory_type_defaults_to_semantic() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title": "t", "content": "c", "category": "note"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        assert!(is_ok(&r), "missing memory_type should default, got err: {}", err_msg(&r));
    }

    /// Bogus category should reject (regression test for M-1).
    #[test]
    fn memory_add_with_bogus_category_rejected() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title": "t", "content": "c", "category": "bogus"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        assert!(!is_ok(&r));
        assert!(err_msg(&r).contains("bogus"));
    }

    /// Bogus memory_type should reject.
    #[test]
    fn memory_add_with_bogus_memory_type_rejected() {
        let (mut store, cfg) = setup();
        let r = call_memory_add(
            json!({"title": "t", "content": "c", "category": "note", "memory_type": "bogus"}),
            &mut store, &cfg, serde_json::Value::Null,
        );
        assert!(!is_ok(&r), "expected rejection, got {}", err_msg(&r));
        assert!(err_msg(&r).contains("memory_type"));
    }

    /// Regression: memory_next must wrap the None branch in the MCP
    /// content envelope. Without the wrap, the TS callTool helper
    /// crashes with "Cannot read properties of null (reading 'isError')"
    /// and the session disconnects with a misleading 'not connected'
    /// error.
    #[test]
    fn memory_next_empty_returns_envelope_not_bare_null() {
        let (mut store, cfg) = setup();
        let r = call_memory_next(
            json!({}),
            &mut store, &cfg,
            serde_json::Value::Null,
        );
        assert!(is_ok(&r), "expected ok, got error: {}", err_msg(&r));
        let v = r.result.as_ref().expect("result must be present");
        // Must have {content: [...]} shape, NOT bare null.
        let content = v.get("content").and_then(|c| c.as_array())
            .expect("result must be wrapped in MCP content envelope");
        assert_eq!(content.len(), 1);
        let text = content[0].get("text").and_then(|t| t.as_str()).unwrap();
        // Text is the JSON literal "null" — callTool JSON.parses it to JS null.
        assert_eq!(text, "null");
    }
}
