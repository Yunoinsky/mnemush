/**
 * mnemush-pi — Pi extension for mnemush memory.
 *
 * Registers:
 *   - Hooks: session_start, session_end, before_agent_start, after_tool_call
 *   - Tools: memory (action=add|search), memory_get, memory_link,
 *          memory_neighbors, memory_save_search_result, memory_reflect,
 *          mnemush_status, identity_propose, identity_review,
 *          identity_approve, identity_reject
 *
 * The extension auto-spawns the mnemush-mcp binary on session start and
 * disconnects on session end. All memory operations are routed through
 * the shared mnemush-client library.
 */

// Pi SDK types are inferred via duck-typing. The extension exports a
// default function that accepts the Pi extension API. We intentionally
// avoid a hard dependency on @earendil-works/pi-coding-agent so the
// package stays buildable in isolation.

import {
  MnemushClient,
  formatMemory,
  formatSearchHit,
  isMnemushTool,
  looksLikeCorrection,
  looksLikeRemember,
  buildConceptInject,
  parseConceptsJson,
  loadConceptInject,
  runSessionEndMaintenance,
  maybeRunDream,
  appendEvalLog,
} from "mnemush-client";

// 概念表 helper 已上移至 mnemush-client(与 mnemush-dsh 共享)。这里再
// 导出一次以保持 mnemush-pi 的公开 API 兼容。
export { buildConceptInject, parseConceptsJson };

interface ToolDefinition {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  execute: (toolCallId: string, args: Record<string, unknown>) => Promise<unknown>;
}

interface ToolResult<T> {
  content: Array<{ type: "text"; text: string }>;
  isError?: boolean;
  details?: T;
}

interface ExtensionAPI {
  registerTool: <T>(tool: ToolDefinition & {
    execute: (toolCallId: string, args: Record<string, unknown>) => Promise<ToolResult<T>>;
  }) => void;
  on: (event: string, handler: (event: unknown, ctx: unknown) => void | Promise<void>) => void;
  // pi runtime's sendMessage takes a CustomMessage object {customType, content,
  // display, details} — NOT a plain string (a string would inject empty content).
  sendMessage?: (
    message: { customType: string; content: string; display?: boolean; details?: unknown },
    options?: { triggerTurn?: boolean; deliverAs?: "steer" | "followUp" | "nextTurn" },
  ) => void;
  sendStatus?: (status: string, ttlMs?: number) => void;
}

let client: MnemushClient | null = null;
let toolCallsThisTurn = 0;

function getClient(): MnemushClient {
  if (!client) {
    throw new Error("mnemush-pi: not connected. Session start hook may have failed.");
  }
  return client;
}

function result<T>(text: string, details?: T): ToolResult<T> {
  return { content: [{ type: "text", text }], details };
}

function err(text: string): ToolResult<never> {
  return {
    content: [{ type: "text", text: `❌ ${text}` }],
    isError: true,
  };
}

// ── Self-eval observability helpers ──────────────────────────────────

let evalSessionId = "unknown";

function setSessionId(id: string) {
  evalSessionId = id;
}

function summarizeArgs(args: Record<string, unknown>): Record<string, unknown> {
  // Truncate long strings to keep log NDJSON small
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(args)) {
    if (typeof v === "string" && v.length > 80) {
      out[k] = v.slice(0, 80) + "…";
    } else if (Array.isArray(v) && v.length > 10) {
      out[k] = `[…${v.length} items]`;
    } else {
      out[k] = v;
    }
  }
  return out;
}

/**
 * 概念表(context priming index)注入文本构建 + CLI 加载。
 * 实现已上移至 mnemush-client(shared by pi / dsh)。
 */

export default function activate(pi: ExtensionAPI): void {
  // ── session_start: connect to mnemush-mcp ─────────────────────
  pi.on("session_start", async () => {
    // Self-eval: generate a per-session id so the NDJSON log file
    // is per-session and easy to inspect.
    setSessionId(`pi-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`);
    try {
      client = await MnemushClient.connect({
        onLog: (msg) => console.error(`[mnemush] ${msg}`),
      });
      pi.sendStatus?.("🧠 mnemush connected", 5000);
      // Surface pending identity proposals so the user (or the agent)
      // doesn't have to remember to check. Fire-and-forget so a slow
      // MCP call doesn't block session start.
      void surfacePendingIdentityProposals();
      // Also surface reflect-candidate count — memories the LLM
      // could review for missing links. Lets the user (or the agent)
      // notice when there's reflection work to do.
      void surfaceReflectCandidateCount();
      // 概念表注入: 唤起索引写入 agent 上下文(连接成功后 fire-and-forget)。
      void injectConceptIndex();
      // 会话驱动 dream 调度(方案 B): 距上次 > 24h 则后台巩固。
      void maybeRunDream({ onOutput: (line) => line && pi.sendStatus?.(`🌙 dream: ${line}`, 8000) });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error(`[mnemush] failed to connect: ${msg}`);
      pi.sendStatus?.(`⚠️ mnemush unavailable: ${msg}`, 10000);
    }
  });

  /**
   * Read pending identity proposals and surface a one-line summary
   * via sendStatus. Runs asynchronously after session_start so it
   * doesn't block the rest of the start sequence. Failures are silent
   * (status messages can pile up; this is a "nice to have").
   *
   * ponytail: spawned as fire-and-forget; status TTL 12s — long enough
   * to be noticed, short enough to not linger into the conversation.
   */
  async function surfacePendingIdentityProposals(): Promise<void> {
    if (!client) return;
    try {
      const list = await client.identityListPending({ status: "pending" });
      if (list.length === 0) return;
      const preview = list
        .slice(0, 3)
        .map((p) => `${p.target.replace(".md", "")}:${p.content.slice(0, 30)}`)
        .join(" | ");
      const more = list.length > 3 ? ` (+${list.length - 3} more)` : "";
      pi.sendStatus?.(
        `🪪 ${list.length} pending identity update${list.length === 1 ? "" : "s"} — run \`mnemush identity list-pending\` to review. ${preview}${more}`,
        12_000,
      );
    } catch {
      // Silent: status updates must never error-loop.
    }
  }

  /**
   * Surface how many memories are candidates for reflection (recent
   * additions with few edges). Lets the user/agent notice when there's
   * connection-finding work to do. Silent on failure.
   */
  async function surfaceReflectCandidateCount(): Promise<void> {
    if (!client) return;
    try {
      const mems = await client.memoryReflect({ sinceDays: 7, limit: 50 });
      if (mems.length === 0) return;
      pi.sendStatus?.(
        `🔍 ${mems.length} memory(ies) worth reviewing for missing links — call \`memory_reflect\` to inspect.`,
        12_000,
      );
    } catch {
      // Silent.
    }
  }

  /**
   * 概念表注入: spawn `mnemush concepts --limit 40 --format json`, 解析后
   * 以 custom message 注入 LLM 上下文(display=false → 不刷聊天 UI)。
   * 失败/空 → 静默。fire-and-forget, 不阻塞会话。
   */
  async function injectConceptIndex(): Promise<void> {
    try {
      const inject = await loadConceptInject();
      if (!inject) return;
      pi.sendMessage?.({
        customType: "memoryIndex",
        content: inject,
        display: false,
      });
    } catch {
      // 注入失败静默 — 会话照常进行。
    }
  }

  /**
   * 检测 memory 写入工具(任何 surface 前缀): memory add/save/action 写入 → 刷新注入。
   * event 结构跨 pi 版本有差异(toolCall.name / toolName / tool_name), 防御性提取。
   */
  function isMemoryWriteTool(name: string, args?: unknown): boolean {
    const n = name ?? "";
    if (n === "memory" || n === "mnemush_memory") {
      return (args as { action?: string } | undefined)?.action === "add";
    }
    return /^(mnemush_)?memory_(add|save_search_result|action_create|action_update)/.test(n);
  }

  /** 写入检测 + 刷新: after_tool_call(旧名) 与 tool_result(pi 0.83) 都注册。 */
  const refreshConceptIndexOnWrite = async (event: unknown): Promise<void> => {
    const e = event as
      | { toolCall?: { name?: string }; toolName?: string; tool_name?: string; input?: unknown; args?: unknown }
      | undefined;
    const tool = e?.toolCall?.name ?? e?.toolName ?? e?.tool_name ?? "";
    if (!isMemoryWriteTool(tool, e?.input ?? e?.args)) return;
    await injectConceptIndex();
  };
  pi.on("after_tool_call", refreshConceptIndexOnWrite);
  pi.on("tool_result", refreshConceptIndexOnWrite);

  // ── session_end: prune + disconnect ─────────────────────────────
  // Active forgetting: before disconnecting, run a prune pass.
  // Default is `apply` (soft-delete up to N candidates); set
  // MNEMUSH_PRUNE_ON_SESSION_END=dry to list candidates without writing,
  // or =off to skip entirely.
  // Hard-delete (`--isolate`) is NEVER auto-run; users opt in manually.
  pi.on("session_end", async () => {
    if (client) {
      try {
        // 共享维护四件套(prune/edge-decay/needs-review/eval-prune)。
        // 门控与输出解析都在这里; 失败静默, 永不阻塞 session_end。
        const results = await runSessionEndMaintenance();
        const pruned = results.get("prune") ?? "";
        const pm = pruned.match(/soft-deleted (\d+) memory/);
        if (pm && pm[1]) {
          pi.sendStatus?.(`🧹 mnemush pruned ${pm[1]} memory(ies) (recoverable: \`mnemush prune --help\`)`, 5000);
        }
        const ed = results.get("edge-decay") ?? "";
        const em = ed.match(/edges decayed: (\d+)/);
        if (em && em[1] && parseInt(em[1], 10) > 0) {
          pi.sendStatus?.(`📈 mnemush edge-decay updated ${em[1]} edge(s)`, 5000);
        }
        const nr = results.get("needs-review") ?? "";
        const nm = nr.match(/needs_review processed: (\d+)/);
        if (nm && nm[1] && parseInt(nm[1], 10) > 0) {
          pi.sendStatus?.(`✅ mnemush needs-review processed ${nm[1]} item(s)`, 5000);
        }
        const ep = results.get("eval-prune") ?? "";
        const evm = ep.match(/removed by age=(\d+), by count=(\d+), lines dropped=(\d+)/);
        if (evm && evm[1] && evm[2] && evm[3]) {
          const age = parseInt(evm[1], 10);
          const count = parseInt(evm[2], 10);
          const dropped = parseInt(evm[3], 10);
          if (age > 0 || count > 0 || dropped > 0) {
            pi.sendStatus?.(`🧹 mnemush eval log: ${age} aged, ${count} overflow, ${dropped} lines dropped`, 5000);
          }
        }
      } catch (e) {
        console.error(`[mnemush] session-end maintenance failed: ${e}`);
      }
      try {
        await client.disconnect();
      } catch (e) {
        console.error(`[mnemush] disconnect error: ${e}`);
      }
      client = null;
    }
  });
  // ── before_agent_start: heuristic capture ──────────────────────
  // Pi's actual event for "user just submitted a prompt" is
  // `before_agent_start` (fires after submit, before agent loop).
  // The earlier `user_prompt_submit` listener was never triggered
  // because that event name does not exist in pi's runtime.
  // `event.prompt` is the user's prompt text directly.
  pi.on("before_agent_start", async (event) => {
    if (!client) return;
    toolCallsThisTurn = 0;
    const e = event as { prompt?: string } | undefined;
    const text = e?.prompt ?? "";
    if (!text) return;
    try {
      if (looksLikeRemember(text)) {
        await client.memoryAdd({
          title: text.slice(0, 80),
          content: text,
          category: "note",
          importance: 0.9,
          source: "auto_heuristic" as never,
        });
        pi.sendStatus?.("🧠 saved (remember)", 3000);
        void injectConceptIndex(); // 启发式写入不走 tool_result 事件, 主动刷新概念表
      } else if (looksLikeCorrection(text)) {
        await client.memoryAdd({
          title: text.slice(0, 80),
          content: text,
          category: "correction",
          importance: 0.9,
          source: "auto_heuristic" as never,
        });
        pi.sendStatus?.("🧠 saved (correction)", 3000);
        void injectConceptIndex(); // 同上: 写入成功后刷新注入
      }
    } catch (e) {
      console.error(`[mnemush] auto-capture failed: ${e}`);
    }
  });

  // ── Periodic insight-save nudge (v0.3) ───────────────────────────
  // Every N tool calls in a turn, surface a reminder that the LLM
  // should use `memory_add(category=insight)` if anything memorable
  // surfaced. Mirrors the auto-capture for "remember this" / "don't"
  // but covers the implicit case (user says "I prefer X" without
  // an explicit remember signal).
  //
  // ponytail: piggyback on existing after_tool_call, not a new hook.
  // Skip our own tools so we don't count ourselves. No new schema,
  // no new tool — just a counter + sendStatus.
  pi.on("after_tool_call", async (event) => {
    const e = event as { tool_name?: string } | undefined;
    const name = e?.tool_name ?? "";
    // Skip tools mnemush registers on any surface (Pi un-prefixed,
    // OpenCode mnemush-/identity- prefixed). Without this, calling
    // `memory` in a Pi session would self-trigger the insight nudge.
    // See isMnemushTool() — prefix match, no hand-maintained list.
    if (isMnemushTool(name)) {
      return;
    }
    toolCallsThisTurn++;
    if (toolCallsThisTurn === 6) {
      pi.sendStatus?.(
        "💡 6 tool calls this turn. If anything memorable surfaced, use `memory_add(category=insight, importance=0.7)`.",
        8000,
      );
    } else if (toolCallsThisTurn === 14) {
      pi.sendStatus?.(
        "💡 14 tool calls. Strong nudge: review the turn. Save any durable insight/preference/decision now.",
        8000,
      );
    }
  });

  // ── after_tool_call: heuristic capture for tool errors ───────
  pi.on("after_tool_call", async (event) => {
    if (!client) return;
    const e = event as { tool_name?: string; result?: { error?: string; is_error?: boolean } } | undefined;
    // Skip mnemush tools (any surface) — we don't want to record our
    // own failures as "tool failure" memories (that's just noise).
    if (!e || !e.tool_name || isMnemushTool(e.tool_name)) return;
    const errorText = e.result?.error;
    if (errorText && e.result?.is_error) {
      try {
        await client.memoryAdd({
          title: `tool failure: ${e.tool_name}`,
          content: `${e.tool_name} failed: ${errorText.slice(0, 200)}`,
          category: "failure",
          importance: 0.7,
          source: "auto_heuristic" as never,
          needs_review: true,
        });
      } catch (err) {
        console.error(`[mnemush] tool-failure save failed: ${err}`);
      }
    }
  });

  // ── after_tool_call: self-eval observability ────────────────
  // Every mnemush-* call is logged to ~/.mnemush/eval/<session>.ndjson
  // for the `mnemush eval stats` command. Foundation for measuring
  // "is mnemush working well" with real data instead of vibes.
  pi.on("after_tool_call", async (event) => {
    const e = event as {
      tool_name?: string;
      args?: Record<string, unknown>;
      result?: { content?: Array<{ text?: string }>; isError?: boolean };
    } | undefined;
    const name = e?.tool_name;
    if (!name) return;
    if (!isMnemushTool(name)) {
      return;
    }
    const t0 = Date.now();
    let error: string | null = null;
    let result_count = 0;
    if (e?.result?.isError) {
      const txt = e.result.content?.[0]?.text;
      error = (txt ?? "error").slice(0, 200);
    } else if (e?.result?.content) {
      // Parse a JSON text body to count results
      try {
        const txt = e.result.content[0]?.text ?? "";
        const parsed = JSON.parse(txt);
        if (Array.isArray(parsed)) {
          result_count = parsed.length;
        } else if (parsed && typeof parsed === "object" && Array.isArray((parsed as any).saved)) {
          result_count = (parsed as any).saved.length;
        }
      } catch { /* not JSON array — keep 0 */ }
    }
    appendEvalLog({
      ts: Math.floor(t0 / 1000),
      session: evalSessionId,
      agent: "pi",
      tool: name,
      args_summary: summarizeArgs(e?.args ?? {}),
      result_count,
      latency_ms: Date.now() - t0,
      error,
    });
  });

  // ── Tools ────────────────────────────────────────────────────

  pi.registerTool({
    name: "memory",
    description:
      "Persistent memory: add or search. " +
      "Action is required. Use this to save decisions, preferences, " +
      "conventions, or anything worth remembering across sessions. " +
      "Also use it to retrieve prior knowledge before answering.",
    parameters: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["add", "search"],
          description: "What to do with the memory.",
        },
        content: { type: "string", description: "Memory content (for add)." },
        title: { type: "string", description: "Short title (for add)." },
        importance: { type: "number", description: "0.0-1.0 (for add). Defaults to 0.5." },
        query: { type: "string", description: "Search query (for search)." },
        category: {
          type: "string",
          enum: ["decision", "lesson", "failure", "correction", "insight", "preference", "convention", "tool_quirk", "episodic", "skill", "identity", "note"],
          description: "Category (for add) or category filter (for search).",
        },
        project: { type: "string", description: "Project filter (for search)." },
        limit: { type: "number", description: "Max results (for search). Default 10." },
      },
      required: ["action"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      const action = args.action as string;
      try {
        switch (action) {
          case "add": {
            if (!args.content || !args.title) {
              return err("add requires both title and content");
            }
            const r = await c.memoryAdd({
              title: args.title as string,
              content: args.content as string,
              category: (args.category as never) ?? "note",
              importance: (args.importance as number) ?? 0.5,
            });
            let out = `✓ added #${r.id.slice(0, 8)}`;
            if (r.conflicts.length > 0) {
              out += `\n🔗 linked ${r.conflicts.length} related memory(ies):\n`;
              for (const c2 of r.conflicts.slice(0, 3)) {
                out += `  - ${c2.title} (${c2.category})\n`;
              }
            }
            return result(out, r);
          }
          case "search": {
            if (!args.query) return err("search requires query");
            const hits = await c.memorySearch(args.query as string, {
              category: args.category as never,
              project: args.project as string | undefined,
              limit: (args.limit as number) ?? 10,
            });
            if (hits.length === 0) return result("(no matches)");
            return result(hits.map(formatSearchHit).join("\n\n"), hits);
          }
          default:
            return err(`unknown action: ${action}`);
        }
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_get",
    description:
      "Fetch a single memory by its full UUID. " +
      "Search hits only expose an 8-char prefix; use this tool to " +
      "retrieve the full id and metadata (e.g. before linking).",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the memory." },
      },
      required: ["id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const m = await c.memoryGet(args.id as string);
        if (!m) return err(`memory not found: ${args.id}`);
        return result(formatMemory(m), m);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_link",
    description:
      "Create or strengthen an edge between two memories. " +
      "Use this to mark related facts, support for a decision, or " +
      "contradiction between two beliefs.",
    parameters: {
      type: "object",
      properties: {
        source_id: { type: "string" },
        target_id: { type: "string" },
        edge_type: {
          type: "string",
          enum: ["related", "supports", "contradicts", "supersedes"],
        },
        strength: { type: "number" },
      },
      required: ["source_id", "target_id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const edge = await c.memoryLink(
          args.source_id as string,
          args.target_id as string,
          (args.edge_type as never) ?? "related",
          (args.strength as number) ?? 0.5,
        );
        return result(`✓ linked #${edge.id.slice(0, 8)} (${edge.edge_type}, strength=${edge.strength})`, edge);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── memory_reflect: surface under-connected recent memories ─────────
  // The agent (LLM) reads the returned candidates and decides which
  // conceptual links the auto-link layer missed. This is layer B of the
  // insight/eureka mechanism — the algorithm does the bookkeeping,
  // the LLM does the judgment.
  // ── memory_save_search_result: explicit save of search hits ─────────
  // Distinct from memory_add in that the input is search-hit ids, not
  // raw content. Use when the user says "remember this" about a search
  // result, or when you (the LLM) want to retain a hit for later.
  // EXPLICIT only — no auto-save.
  pi.registerTool({
    name: "memory_save_search_result",
    description:
      "Explicitly save one or more search hits as memories. Pass the memory " +
      "ids returned by a prior memory_search call; each becomes a memory " +
      "with the original content and a 'saved from search: <query>' context " +
      "line. Use this when the user wants to keep a result, or when you " +
      "notice a hit worth retaining for later. NEVER auto-save; only call " +
      "this in response to an explicit signal.",
    parameters: {
      type: "object",
      properties: {
        ids: { type: "array", items: { type: "string" } },
        query: { type: "string", description: "The original search query (recorded in the context)." },
        category: { type: "string", default: "note" },
        importance: { type: "number", default: 0.5 },
      },
      required: ["ids", "query"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const out = await c.memorySaveSearchResult({
          ids: args.ids as string[],
          query: args.query as string,
          category: args.category as string | undefined,
          importance: args.importance as number | undefined,
        });
        const n = out.saved?.length ?? 0;
        const errs = out.errors ?? [];
        const msg = errs.length > 0
          ? `saved ${n}, errors: ${errs.join("; ")}`
          : `saved ${n} memory(ies)`;
        return result(msg, out);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_reflect",
    description:
      "Surface recent, under-connected memories for LLM-driven reflection. " +
      "Returns the candidate memories (sorted by low edge count, then " +
      "most recent). For each candidate, decide whether it conceptually " +
      "links to other recent memories; if so, call memory_link to add the " +
      "edge. Optionally write an 'insight' memory describing the connection. " +
      "Use this when you want to find conceptual links the auto-link layer " +
      "missed (it only catches literal word overlap).",
    parameters: {
      type: "object",
      properties: {
        since_days: { type: "number", default: 7 },
        limit: { type: "number", default: 20 },
      },
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const mems = await c.memoryReflect({
          sinceDays: args.since_days as number | undefined,
          limit: args.limit as number | undefined,
        });
        if (mems.length === 0) {
          return result("(no candidates)", []);
        }
        const lines = mems.map(
          (m) => `- #${m.id.slice(0, 8)}  [${m.category}|imp=${m.importance.toFixed(2)}]  ${m.title}\n  ${m.content.slice(0, 120)}`,
        );
        return result(
          `${mems.length} candidate(s):\n${lines.join("\n")}\n\nFor each, decide if it links to any other (use memory_neighbors to inspect, memory_link to connect).`,
          mems,
        );
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── memory_neighbors: walk 1-hop graph from a memory ────────
  // Discover related memories via existing edges. Returns each
  // neighbor's id, hop distance, and a short preview so the LLM can
  // decide whether to call memory_link to add a missing edge or
  // memory_get to read the full content. Default 2 hops follows the
  // spreading activation configuration.
  pi.registerTool({
    name: "memory_neighbors",
    description:
      "Walk the memory graph from a given id, returning each neighbor " +
      "with its hop distance (1..max_hops) and a short preview. Useful " +
      "before memory_link to see what already exists.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Memory id to start from" },
        max_hops: {
          type: "number",
          default: 2,
          description: "Max hop distance (1-5). Default 2.",
        },
      },
      required: ["id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const id = args.id as string;
        const maxHops = (args.max_hops as number) ?? 2;
        const hits = await c.memoryNeighbors(id, maxHops);
        if (hits.length === 0) {
          return result("(no neighbors)", []);
        }
        const lines = hits.map(
          (h) =>
            `[hop ${h.hop}] #${h.memory.id.slice(0, 8)}  ${h.memory.title}\n     ${h.memory.content.slice(0, 100)}`,
        );
        return result(`${hits.length} neighbor(s):\n${lines.join("\n")}`, hits);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── mnemush_status: one-line system state summary ────────────────
  pi.registerTool({
    name: "mnemush_status",
    description:
      "One-line summary of memory system state: active memories, soft-deleted " +
      "memories, edges, needs_review, prune candidates (matching should_prune), " +
      "reflect candidates, pending identity proposals. Call this when you want to " +
      "know the overall state without running multiple commands.",
    parameters: { type: "object", properties: {} },
    execute: async (_id, _args) => {
      const c = getClient();
      try {
        const s = await c.mnemushStatus();
        return result(
          `mnemush status: active=${s.active} soft-deleted=${s.soft_deleted} edges=${s.edges} ` +
            `needs_review=${s.needs_review} prune_candidates=${s.prune_candidates} ` +
            `reflect_candidates=${s.reflect_candidates} pending_proposals=${s.pending_proposals}`,
          s,
        );
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── identity reflection (v0.2) ────────────────────────────────────
  // The LLM observes behavior across sessions and proposes updates to
  // USER.md / PERSONA.md. The user reviews with `identity_review` and
  // applies with `identity_approve` / `identity_reject`. The LLM MUST
  // call `identity_propose` rather than writing to the identity files
  // directly — updates are never applied silently.

  pi.registerTool({
    name: "identity_propose",
    description:
      "Propose an update to one of the identity files (USER.md / PERSONA.md / " +
      "CONSTITUTION.md). The proposal is queued for the user to review. " +
      "Call this when you have a high-confidence observation about the user " +
      "(e.g. their role, preferences, project) that the current identity " +
      "files don't yet capture. Provide a clear reason and an evidence_count " +
      "(how many distinct observations support the proposal). NEVER write to " +
      "the identity files directly — always go through this tool.",
    parameters: {
      type: "object",
      properties: {
        target: {
          type: "string",
          enum: ["USER.md", "PERSONA.md", "CONSTITUTION.md"],
        },
        content: { type: "string", description: "The proposed content to append" },
        reason: { type: "string", description: "Why this is being proposed" },
        evidence_count: {
          type: "number",
          default: 1,
          description: "Number of distinct observations supporting this",
        },
      },
      required: ["target", "content", "reason"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const p = await c.identityPropose({
          target: args.target as "USER.md" | "PERSONA.md" | "CONSTITUTION.md",
          content: args.content as string,
          reason: args.reason as string,
          evidenceCount: args.evidence_count as number | undefined,
        });
        const short = p.id.slice(0, 8);
        return result(
          `✓ proposed #${short} → ${p.target}\n  reason: ${p.reason}\n  evidence: ${p.evidence_count}\n  The user will see this in their next 'mnemush identity list-pending' run.`,
          p,
        );
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "identity_review",
    description:
      "List pending identity-update proposals so the user (or you, when " +
      "instructed) can review them. Returns id, target, content, reason, " +
      "and evidence_count for each. Pair with `mnemush identity approve <id>` " +
      "or `mnemush identity reject <id>` to act on a proposal.",
    parameters: {
      type: "object",
      properties: {
        status: { type: "string", enum: ["pending", "approved", "rejected"] },
        all: { type: "boolean", default: false },
      },
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const list = await c.identityListPending({
          status: args.status as "pending" | "approved" | "rejected" | undefined,
          all: args.all as boolean | undefined,
        });
        if (list.length === 0) {
          return result("(no proposals)", []);
        }
        const lines = list.map(
          (p) =>
            `- #${p.id.slice(0, 8)}  [${p.status}|ev=${p.evidence_count}]  → ${p.target}\n  ${p.content}\n  reason: ${p.reason}`,
        );
        return result(`${list.length} proposal(s):\n${lines.join("\n")}`, list);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "identity_approve",
    description:
      "Approve a pending identity-update proposal. Use the **full id** " +
      "(returned by identity_review). The proposal's `content` is appended " +
      "to the target file with a dated header; the original is preserved. " +
      "Use this when both you and the user consider the proposal safe.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the proposal to approve" },
      },
      required: ["id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const p = await c.identityApprove(args.id as string);
        if (!p) return err(`proposal ${args.id} not found or already resolved`);
        return result(
          `✓ approved #${p.id.slice(0, 8)} → ${p.target}\n  ${p.content}`,
          p,
        );
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "identity_reject",
    description:
      "Reject a pending identity-update proposal. The proposal is marked " +
      "rejected (no file change). Use this when the proposal is wrong, " +
      "premature, or duplicative. Use the **full id** from identity_review.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the proposal to reject" },
      },
      required: ["id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const p = await c.identityReject(args.id as string);
        if (!p) return err(`proposal ${args.id} not found or already resolved`);
        return result(`✓ rejected #${p.id.slice(0, 8)} (${p.target})`, p);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── v0.3 agent self-memory (commitments / actions) ────────────────
  // The agent tracks its own outstanding work as memories with status
  // active|completed|abandoned. memory_next returns the highest-
  // priority commitment (deadline soonest, then newest). The server
  // auto-manages completed_at on terminal transitions.

  pi.registerTool({
    name: "memory_next",
    description:
      "Return the highest-priority active commitment (status=active, " +
      "category != 'identity'), or null if none exist. Priority order: " +
      "due_at ASC (nulls last), then created_at DESC, then id DESC for " +
      "stable tie-break. Use this when the user asks 'what should you " +
      "be working on?' or to pick up after a context boundary.",
    parameters: { type: "object", properties: {} },
    execute: async (_id) => {
      const c = getClient();
      try {
        const m = await c.memoryNext();
        if (!m) return result("(no active commitments)", null);
        return result(`#${m.id.slice(0, 8)}  ${m.title}`, m);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_frontier",
    description:
      "Return all active commitments (status=active, category != 'identity'), " +
      "ordered by due_at ASC (nulls last), then created_at DESC. Use for " +
      "'show me everything I'm working on' overviews.",
    parameters: { type: "object", properties: {} },
    execute: async (_id) => {
      const c = getClient();
      try {
        const mems = await c.memoryFrontier();
        if (mems.length === 0) return result("(no active commitments)", []);
        const lines = mems.map((m) =>
          `- #${m.id.slice(0, 8)}  imp=${m.importance.toFixed(2)}  ${m.due_at ? `due=${m.due_at}  ` : ""}${m.title}`,
        );
        return result(`${mems.length} active:\n${lines.join("\n")}`, mems);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_action_create",
    description:
      "Create a commitment (work the agent owes). Distinct from " +
      "memory_add in that the result is implicitly status=active and " +
      "treated as an action (excluded from memory_next only when " +
      "category=='identity'). Pass due_at as a unix timestamp in seconds " +
      "for time-bound work. The server returns the created Memory " +
      "with its id.",
    parameters: {
      type: "object",
      properties: {
        title: { type: "string" },
        content: { type: "string" },
        importance: { type: "number", default: 0.5 },
        due_at: { type: "number", description: "Unix timestamp (seconds). Omit for no deadline." },
        claimed_by: { type: "string", description: "Session or owner claiming the work." },
        parent_id: { type: "string", description: "Parent commitment id for hierarchical work." },
        tags: { type: "array", items: { type: "string" } },
      },
      required: ["title", "content"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const m = await c.memoryActionCreate({
          title: args.title as string,
          content: args.content as string,
          importance: args.importance as number | undefined,
          due_at: args.due_at as number | undefined,
          claimed_by: args.claimed_by as string | undefined,
          parent_id: args.parent_id as string | undefined,
          tags: args.tags as string[] | undefined,
        });
        return result(`✓ created #${m.id.slice(0, 8)}  ${m.title}`, m);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  pi.registerTool({
    name: "memory_action_update",
    description:
      "Update a commitment. On status transition to 'completed' or " +
      "'abandoned' the server auto-sets completed_at; on transition " +
      "back to 'active' the server clears it. The returned Memory " +
      "reflects the post-write state.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string" },
        status: { type: "string", enum: ["active", "completed", "abandoned"] },
        due_at: { type: "number", description: "Unix timestamp (seconds). Pass null to clear." },
        claimed_by: { type: "string", description: "Pass null to unclaim." },
        importance: { type: "number" },
      },
      required: ["id"],
    },
    execute: async (_id, args) => {
      const c = getClient();
      try {
        const m = await c.memoryActionUpdate({
          id: args.id as string,
          status: args.status as "active" | "completed" | "abandoned" | undefined,
          due_at: args.due_at as number | null | undefined,
          claimed_by: args.claimed_by as string | null | undefined,
          importance: args.importance as number | undefined,
        });
        const completedTag = m.completed_at ? `  completed_at=${m.completed_at}` : "";
        return result(`✓ #${m.id.slice(0, 8)}  status=${m.status}${completedTag}`, m);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });
}
