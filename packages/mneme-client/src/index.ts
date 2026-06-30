// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

/**
 * mneme-client — TypeScript client for the mneme MCP server.
 *
 * Spawns the `mneme-mcp` binary (or `mneme mcp` from the same crate)
 * and speaks JSON-RPC 2.0 over stdio. This client is shared by the
 * Pi extension and the OpenCode plugin.
 *
 * Usage:
 *   const client = await MnemeClient.connect();
 *   const id = await client.memoryAdd({ content: "...", title: "..." });
 *   const hits = await client.memorySearch("auth", { limit: 5 });
 *   await client.disconnect();
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

// ── Types ──────────────────────────────────────────────────────

export type Category =
  | "decision"
  | "lesson"
  | "failure"
  | "correction"
  | "insight"
  | "preference"
  | "convention"
  | "tool_quirk"
  | "episodic"
  | "skill"
  | "identity"
  | "note";

export type MemoryType = "identity" | "procedural" | "semantic";

export type EdgeType = "related" | "supports" | "contradicts" | "supersedes";

export type Source =
  | "manual"
  | "auto_heuristic"
  | "auto_review"
  | "correction"
  | "skill"
  | "session_import"
  | "search_result";

export interface Memory {
  id: string;
  memory_type: MemoryType;
  tier: "global" | "project" | "skill" | "session";
  category: Category;
  title: string;
  content: string;
  context: string | null;
  topic_key: string | null;
  tags: string[];
  project: string | null;
  source: Source;
  initial_confidence: number;
  confidence: number;
  importance: number;
  access_count: number;
  last_accessed_at: string;
  created_at: string;
  override_half_life: number | null;
  never_prune: boolean;
  never_decay: boolean;
  content_hash: string;
  deleted_at: string | null;
  needs_review: boolean;
}

export interface SearchHit {
  memory: Memory;
  score: number;
  bm25: number;
  retrievability: number;
}

export interface Edge {
  id: string;
  source_id: string;
  target_id: string;
  edge_type: EdgeType;
  strength: number;
  initial_strength: number;
  bidirectional: boolean;
  provenance: string | null;
  evidence: string | null;
  context: string | null;
  access_count: number;
  last_activated: string | null;
  stability: number;
  created_at: string;
  deleted_at: string | null;
}

export interface NeighborHit {
  memory: Memory;
  hop: number;
}

export interface AddOptions {
  title: string;
  content: string;
  category?: Category;
  memory_type?: MemoryType;
  importance?: number;
  tags?: string[];
  project?: string;
  context?: string;
  needs_review?: boolean;
  source?: Source;
}

export interface AddResult {
  id: string;
  conflicts: Memory[];
}

export interface SearchOptions {
  category?: Category;
  memory_type?: MemoryType;
  project?: string;
  limit?: number;
}

// ── Display helpers (shared by Pi and OpenCode plugins) ──────────

/** Render a Memory as a human-readable multi-line block. */
export function formatMemory(m: Memory): string {
  const lines: string[] = [];
  lines.push(`#${m.id.slice(0, 8)}  ${m.title}`);
  lines.push(
    `  type=${m.memory_type}  category=${m.category}  importance=${m.importance.toFixed(2)}`,
  );
  if (m.tags.length > 0) lines.push(`  tags: ${m.tags.join(", ")}`);
  if (m.topic_key) lines.push(`  topic: ${m.topic_key}`);
  lines.push(`  ${m.content}`);
  if (m.context) lines.push(`  context: ${m.context}`);
  return lines.join("\n");
}

/** Render a SearchHit as `[score] <formatted memory>`. */
export function formatSearchHit(h: SearchHit): string {
  return `[${h.score.toFixed(2)}] ${formatMemory(h.memory)}`;
}

/**
 * Heuristic patterns for auto-capturing user messages. Substring match
 * (no `\b` boundary) because `\b` in JS only matches ASCII word
 * boundaries and silently fails for every CJK keyword.
 *
 * Lists are intentionally short and high-signal. v0.2's periodic LLM
 * review (see ROADMAP) will pick up everything these miss.
 */

const REMEMBER_RE =
  /(记住|记一下|记得|备忘|重要|remember|don['’]t forget|important|note that|key point)/iu;

const CORRECTION_RE =
  /(不要|别用|错了|不对|更正|应该是|改用|actually|never use|use \w+ not \w+)/iu;

/** Does this user message look like an explicit "remember this"? */
export function looksLikeRemember(text: string): boolean {
  return REMEMBER_RE.test(text);
}

/** Does this user message look like a correction / override? */
export function looksLikeCorrection(text: string): boolean {
  return CORRECTION_RE.test(text);
}

// ── JSON-RPC plumbing ──────────────────────────────────────────

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params: Record<string, unknown>;
}

interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

// ── Client ─────────────────────────────────────────────────────

export interface MnemeClientOptions {
  /** Path to the mneme-mcp binary. Auto-detected if omitted. */
  binaryPath?: string;
  /** Custom data dir (overrides ~/.mneme). */
  dataDir?: string;
  /** Logger callback for diagnostics. */
  onLog?: (msg: string) => void;
}

export class MnemeClient {
  private proc: ChildProcess | null = null;
  private nextId = 1;
  private pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();
  private buffer = "";
  private closed = false;
  private readonly options: MnemeClientOptions;

  constructor(options: MnemeClientOptions = {}) {
    this.options = options;
  }

  /**
   * Spawn the mneme-mcp binary and return a connected client.
   */
  static async connect(options: MnemeClientOptions = {}): Promise<MnemeClient> {
    const c = new MnemeClient(options);
    await c.start();
    return c;
  }

  /**
   * Start the underlying MCP subprocess. Called automatically by
   * `connect()`.
   */
  async start(): Promise<void> {
    if (this.proc) return;

    const bin = this.options.binaryPath ?? findMnemeBinary();
    if (!bin) {
      throw new Error(
        "mneme-mcp binary not found. Install via `cargo install mneme` " +
          "or set MNEME_BINARY env var / pass { binaryPath }.",
      );
    }
    this.options.onLog?.(`spawning ${bin}`);

    const env: NodeJS.ProcessEnv = { ...process.env };
    if (this.options.dataDir) {
      env.MNEME_DATA_DIR = this.options.dataDir;
    }

    this.proc = spawn(bin, [], {
      env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.proc.stdout!.setEncoding("utf-8");
    this.proc.stdout!.on("data", (chunk: string) => this.onStdout(chunk));
    this.proc.stderr!.setEncoding("utf-8");
    this.proc.stderr!.on("data", (chunk: string) => {
      this.options.onLog?.(`[mneme stderr] ${chunk.trim()}`);
    });
    this.proc.on("exit", (code) => {
      this.closed = true;
      const err = new Error(`mneme-mcp exited with code ${code}`);
      for (const { reject } of this.pending.values()) reject(err);
      this.pending.clear();
    });
    this.proc.on("error", (err) => {
      this.options.onLog?.(`[mneme error] ${err.message}`);
    });

    // Initialize the MCP session.
    await this.rpc("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "mneme-client", version: "0.1.0" },
    });
    // Fire-and-forget notification.
    this.notify("notifications/initialized", {});
  }

  private onStdout(chunk: string): void {
    this.buffer += chunk;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      let parsed: JsonRpcResponse;
      try {
        parsed = JSON.parse(line) as JsonRpcResponse;
      } catch (e) {
        this.options.onLog?.(`[mneme bad-json] ${line}`);
        continue;
      }
      const handler = this.pending.get(parsed.id);
      if (!handler) continue;
      this.pending.delete(parsed.id);
      if (parsed.error) {
        handler.reject(new Error(`mneme-mcp error ${parsed.error.code}: ${parsed.error.message}`));
      } else {
        handler.resolve(parsed.result);
      }
    }
  }

  private rpc(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (!this.proc || this.closed) {
      return Promise.reject(new Error("mneme-mcp not connected"));
    }
    const id = this.nextId++;
    const req: JsonRpcRequest = { jsonrpc: "2.0", id, method, params };
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc!.stdin!.write(JSON.stringify(req) + "\n");
    });
  }

  private notify(method: string, params: Record<string, unknown>): void {
    if (!this.proc || this.closed) return;
    const req = { jsonrpc: "2.0" as const, method, params };
    this.proc.stdin!.write(JSON.stringify(req) + "\n");
  }

  // ── Tool calls ──────────────────────────────────────────────

  async memoryAdd(opts: AddOptions): Promise<AddResult> {
    const args: Record<string, unknown> = {
      title: opts.title,
      content: opts.content,
    };
    if (opts.category) args.category = opts.category;
    if (opts.memory_type) args.memory_type = opts.memory_type;
    if (opts.importance !== undefined) args.importance = opts.importance;
    if (opts.tags) args.tags = opts.tags;
    if (opts.project) args.project = opts.project;
    if (opts.context) args.context = opts.context;
    if (opts.needs_review !== undefined) args.needs_review = opts.needs_review;
    if (opts.source) args.source = opts.source;
    return (await this.callTool("memory_add", args)) as AddResult;
  }

  async memorySearch(query: string, opts: SearchOptions = {}): Promise<SearchHit[]> {
    const args: Record<string, unknown> = { query };
    if (opts.category) args.category = opts.category;
    if (opts.memory_type) args.memory_type = opts.memory_type;
    if (opts.project) args.project = opts.project;
    if (opts.limit !== undefined) args.limit = opts.limit;
    return (await this.callTool("memory_search", args)) as SearchHit[];
  }

  async memoryGet(id: string): Promise<Memory | null> {
    try {
      return (await this.callTool("memory_get", { id })) as Memory;
    } catch (e) {
      if (e instanceof Error && /not found/i.test(e.message)) return null;
      throw e;
    }
  }

  async memoryLink(
    sourceId: string,
    targetId: string,
    edgeType: EdgeType = "related",
    strength = 0.5,
  ): Promise<Edge> {
    return (await this.callTool("memory_link", {
      source_id: sourceId,
      target_id: targetId,
      edge_type: edgeType,
      strength,
    })) as Edge;
  }

  async memoryNeighbors(id: string, maxHops = 2): Promise<NeighborHit[]> {
    return (await this.callTool("memory_neighbors", {
      id,
      max_hops: maxHops,
    })) as NeighborHit[];
  }

  private async callTool(name: string, args: Record<string, unknown>): Promise<unknown> {
    const result = (await this.rpc("tools/call", { name, arguments: args })) as {
      content: Array<{ type: string; text: string }>;
      isError?: boolean;
    };
    if (result.isError) {
      throw new Error(`tool ${name} failed: ${result.content?.[0]?.text ?? "unknown"}`);
    }
    const text = result.content?.[0]?.text;
    if (text === undefined) return null;
    try {
      return JSON.parse(text);
    } catch {
      return text;
    }
  }

  async disconnect(): Promise<void> {
    if (this.proc && !this.closed) {
      this.proc.stdin!.end();
      // Give it a moment to exit cleanly.
      await new Promise<void>((resolve) => {
        if (!this.proc) return resolve();
        this.proc.once("exit", () => resolve());
        setTimeout(() => {
          if (this.proc && !this.closed) {
            this.proc.kill("SIGTERM");
          }
          resolve();
        }, 200);
      });
    }
    this.proc = null;
    this.closed = true;
  }
}

// ── Helpers ─────────────────────────────────────────────────────

/**
 * Find the mneme-mcp binary. Checks in order:
 *   1. MNEME_BINARY env var
 *   2. ~/.cargo/bin/mneme-mcp
 *   3. /usr/local/bin/mneme-mcp
 *   4. ./target/release/mneme-mcp (development)
 *   5. ./target/debug/mneme-mcp (development)
 */
export function findMnemeBinary(): string | null {
  if (process.env.MNEME_BINARY && existsSync(process.env.MNEME_BINARY)) {
    return process.env.MNEME_BINARY;
  }
  const home = homedir();
  const candidates = [
    join(home, ".cargo", "bin", "mneme-mcp"),
    "/usr/local/bin/mneme-mcp",
    "/opt/homebrew/bin/mneme-mcp",
    "./target/release/mneme-mcp",
    "./target/debug/mneme-mcp",
    "./crates/mneme/target/release/mneme-mcp",
    "./crates/mneme/target/debug/mneme-mcp",
  ];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  return null;
}
