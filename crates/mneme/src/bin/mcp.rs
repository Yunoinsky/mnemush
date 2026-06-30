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
        }
    ])
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
    let category = p
        .get("category")
        .and_then(|v| v.as_str())
        .and_then(Category::parse)
        .unwrap_or(Category::Note);
    let memory_type = p
        .get("memory_type")
        .and_then(|v| v.as_str())
        .and_then(MemoryType::parse)
        .unwrap_or(MemoryType::Semantic);
    let importance = p.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
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
    if source_id.is_none() || target_id.is_none() {
        return JsonRpcResponse::err(id, -32602, "missing source_id or target_id");
    }
    let edge_type = p
        .get("edge_type")
        .and_then(|v| v.as_str())
        .and_then(EdgeType::parse)
        .unwrap_or(EdgeType::Related);
    let strength = p.get("strength").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let edge_api = EdgeApi::new(store, config);
    match edge_api.link(
        source_id.unwrap(),
        target_id.unwrap(),
        edge_type,
        strength,
        None,
        None,
    ) {
        Ok(e) => json_ok(id, &e),
        Err(err) => json_err(id, err.to_string()),
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
