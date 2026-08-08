/**
 * mnemush-opencode — OpenCode plugin for mnemush memory.
 *
 * Same architecture as the Pi extension. OpenCode's plugin API uses
 * different event names (chat.message, tool.execute.after) so we
 * provide thin shims that adapt to mnemush-client.
 *
 * To install globally:
 *   npm install -g mnemush-opencode
 * Then symlink dist/index.js into ~/.config/opencode/plugin/ or use the
 * auto-discovery script in scripts/install.sh.
 */

import {
  MnemushClient,
  formatMemory,
  formatSearchHit,
  isMnemushTool,
  looksLikeCorrection,
  looksLikeRemember,
  runSessionEndMaintenance,
  appendEvalLog,
} from "mnemush-client";

interface OpenCodeTool {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  execute: (args: Record<string, unknown>, ctx: unknown) => Promise<unknown>;
}

interface OpenCodeClient {
  tool: (def: OpenCodeTool) => void;
  on: (event: string, handler: (event: unknown, ctx: unknown) => void | Promise<void>) => void;
}

let client: MnemushClient | null = null;

function getClient(): MnemushClient {
  if (!client) throw new Error("mnemush-opencode: not connected");
  return client;
}

function result(text: string, data?: unknown): { content: Array<{ type: "text"; text: string }>; data?: unknown } {
  const r: { content: Array<{ type: "text"; text: string }>; data?: unknown } = {
    content: [{ type: "text", text }],
  };
  if (data !== undefined) r.data = data;
  return r;
}

function err(text: string): { content: Array<{ type: "text"; text: string }>; isError: true } {
  return { content: [{ type: "text", text: `❌ ${text}` }], isError: true };
}

interface OpenCodePlugin {
  (ctx: { client: OpenCodeClient; $?: unknown; directory?: string }): void | Promise<void>;
}

const plugin: OpenCodePlugin = ({ client: oc }) => {
  // ── chat.message: heuristic capture ──────────────────────────
  oc.on("chat.message", async (event) => {
    if (!client) return;
    const e = event as { role?: string; content?: string } | undefined;
    if (e?.role !== "user" || !e.content) return;
    const text = e.content;
    try {
      if (looksLikeRemember(text)) {
        await client.memoryAdd({
          title: text.slice(0, 80),
          content: text,
          category: "note",
          importance: 0.9,
          source: "auto_heuristic",
        });
      } else if (looksLikeCorrection(text)) {
        await client.memoryAdd({
          title: text.slice(0, 80),
          content: text,
          category: "correction",
          importance: 0.9,
          source: "auto_heuristic",
        });
      }
    } catch (err) {
      console.error(`[mnemush] auto-capture failed: ${err}`);
    }
  });

  // ── tool.execute.after: capture tool failures ───────────────
  oc.on("tool.execute.after", async (event) => {
    if (!client) return;
    const e = event as { name?: string; result?: { error?: string; is_error?: boolean } } | undefined;
    // Skip mnemush tools (any surface) — don't record our own failures
    // as "tool failure" memories. isMnemushTool() covers all 16 tools
    // (the old check only skipped two).
    if (!e || !e.name || isMnemushTool(e.name)) return;
    const errorText = e.result?.error;
    if (errorText && e.result?.is_error) {
      try {
        await client.memoryAdd({
          title: `tool failure: ${e.name}`,
          content: `${e.name} failed: ${errorText.slice(0, 200)}`,
          category: "failure",
          importance: 0.7,
          source: "auto_heuristic",
          needs_review: true,
        });
      } catch (err) {
        console.error(`[mnemush] tool-failure save failed: ${err}`);
      }
    }
  });

  // ── session lifecycle: connect / disconnect ─────────────────
  // OpenCode doesn't expose explicit start/end events; the plugin
  // function itself runs at startup. We connect lazily on first tool
  // call to avoid blocking plugin load.

  async function ensureConnected() {
    if (client) return;
    client = await MnemushClient.connect({
      onLog: (msg) => console.error(`[mnemush] ${msg}`),
    });
  }

  /**
   * Wrap an MCP-side message in OpenCode's tool result envelope so the
   * LLM sees a clear "result" or "isError: true" path. The OpenCode
   * plugin surface already follows the same shape as the MCP server
   * (text + isError), so the client throws on isError and we just
   * surface the throw here.
   */
  function tryRun(label: string, fn: () => Promise<unknown>) {
    return fn().then(
      (v) => v,
      (e) => err(`${label}: ${e instanceof Error ? e.message : String(e)}`),
    );
  }

  // ── self-eval log (same format as the Pi extension) ──────────
  // OpenCode's tool.execute.after event lacks args/latency, so we
  // instrument the one chokepoint every tool passes through — tool
  // registration. registerTool wraps each execute() to record tool
  // name, latency, and errors to ~/.mnemush/eval/<session>.ndjson,
  // keeping `mnemush eval stats` covering both surfaces. result_count
  // stays 0 (OpenCode doesn't expose parsed result sizes here).
  let ocSessionId = `opencode-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

  /**
   * Register a tool with self-eval instrumentation. Wraps execute()
   * so every tool (not just those that use tryRun) is recorded in the
   * NDJSON log. This is the single chokepoint for the OpenCode surface.
   */
  function registerTool(def: OpenCodeTool): void {
    const inner = def.execute;
    oc.tool({
      ...def,
      execute: async (args, ctx) => {
        const t0 = Date.now();
        const tool = def.name;
        try {
          const r = await inner(args, ctx);
          const typed = r as { isError?: boolean; content?: Array<{ text?: string }> } | undefined;
          void appendEvalLog({
            ts: Math.floor(t0 / 1000),
            session: ocSessionId,
            agent: "opencode",
            tool,
            result_count: 0,
            latency_ms: Date.now() - t0,
            error: typed?.isError
              ? (typed.content?.[0]?.text ?? "error").slice(0, 200)
              : null,
          });
          return r;
        } catch (e) {
          void appendEvalLog({
            ts: Math.floor(t0 / 1000),
            session: ocSessionId,
            agent: "opencode",
            tool,
            result_count: 0,
            latency_ms: Date.now() - t0,
            error: (e instanceof Error ? e.message : String(e)).slice(0, 200),
          });
          throw e;
        }
      },
    });
  }

  // ── session lifecycle ───────────────────────────────────────
  // OpenCode doesn't expose explicit "session_start" / "session_end",
  // but it does emit "session.created" and "session.deleted" (plus
  // "session.idle" when the user pauses). We map these to our pipeline:
  //   session.created → connect + surface pending identity proposals
  //   session.deleted → run prune + edge-decay + needs-review
  // The Pi extension also has "session_end" doing the same — the
  // difference here is event-name mapping.

  oc.on("session.created", async () => {
    try {
      await ensureConnected();
      const c = getClient();
      // Fire-and-forget: surface pending identity proposals so the
      // user (or agent) sees them on session start.
      c.identityListPending()
        .then((list) => {
          if (list.length > 0) {
            const preview = list
              .slice(0, 3)
              .map(
                (p) =>
                  `  - #${p.id.slice(0, 8)} [${p.status}] → ${p.target}\n    ${p.content.slice(0, 80)}`,
              )
              .join("\n");
            console.log(
              `[mnemush] session.created: ${list.length} pending identity proposal(s):\n${preview}`,
            );
          }
        })
        .catch(() => {});
    } catch (e) {
      console.error(`[mnemush] session.created failed: ${e}`);
    }
  });

  oc.on("session.deleted", async () => {
    if (!client) return;
    try {
      // 共享维护四件套(prune/edge-decay/needs-review/eval-prune),
      // 门控与 Pi 插件一致(MNEMUSH_*_ON_SESSION_END)。硬删永不自动。
      await runSessionEndMaintenance();
    } catch (e) {
      console.error(`[mnemush] session.deleted failed: ${e}`);
    }
  });

  // ── Tools ────────────────────────────────────────────────────

  registerTool({
    name: "mnemush-memory",
    description:
      "Persistent memory. Action: add | search. " +
      "Use to save decisions, preferences, conventions, or any " +
      "durable context. Also use to retrieve prior knowledge.",
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["add", "search"] },
        title: { type: "string" },
        content: { type: "string" },
        category: { type: "string" },
        importance: { type: "number" },
        query: { type: "string" },
        limit: { type: "number" },
      },
      required: ["action"],
    },
    execute: async (args) => {
      await ensureConnected();
      const c = getClient();
      const action = args.action as string;
      try {
        if (action === "add") {
          if (!args.content || !args.title) return err("add requires title and content");
          const r = await c.memoryAdd({
            title: args.title as string,
            content: args.content as string,
            category: (args.category as never) ?? "note",
            importance: (args.importance as number) ?? 0.5,
          });
          return result(`✓ added id=${r.id}`);
        }
        if (action === "search") {
          if (!args.query) return err("search requires query");
          const hits = await c.memorySearch(args.query as string, {
            limit: (args.limit as number) ?? 10,
          });
          if (hits.length === 0) return result("(no matches)");
          return result(hits.map(formatSearchHit).join("\n\n"));
        }
        return err(`unknown action: ${action}`);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  registerTool({
    name: "mnemush-memory-search",
    description: "Search long-term memory. Returns ranked hits.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string" },
        category: { type: "string" },
        limit: { type: "number" },
      },
      required: ["query"],
    },
    execute: async (args) => {
      await ensureConnected();
      const c = getClient();
      try {
        const hits = await c.memorySearch(args.query as string, {
          category: args.category as never,
          limit: (args.limit as number) ?? 10,
        });
        if (hits.length === 0) return result("(no matches)");
        return result(hits.map(formatSearchHit).join("\n\n"));
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  registerTool({
    name: "mnemush-memory-link",
    description: "Create an edge between two memories.",
    parameters: {
      type: "object",
      properties: {
        source_id: { type: "string" },
        target_id: { type: "string" },
        edge_type: { type: "string", enum: ["related", "supports", "contradicts", "supersedes"] },
        strength: { type: "number" },
      },
      required: ["source_id", "target_id"],
    },
    execute: async (args) => {
      const sourceId = args.source_id as string | undefined;
      const targetId = args.target_id as string | undefined;
      if (!sourceId || !targetId) {
        return err("link requires both source_id and target_id");
      }
      await ensureConnected();
      const c = getClient();
      try {
        const edge = await c.memoryLink(
          sourceId,
          targetId,
          (args.edge_type as never) ?? "related",
          typeof args.strength === "number" ? args.strength : 0.5,
        );
        return result(`✓ linked id=${edge.id} (${edge.edge_type})`);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  // ── Tools (post-Pi-parity additions) ─────────────────────────

  registerTool({
    name: "mnemush-memory-get",
    description:
      "Fetch a single memory by its full UUID. Search hits only " +
      "expose an 8-char prefix; use this tool to retrieve the full " +
      "id and metadata (e.g. before linking).",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the memory." },
      },
      required: ["id"],
    },
    execute: async (args) => {
      const id = args.id as string | undefined;
      if (!id) return err("memory_get requires id");
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_get failed", async () => {
        const m = await c.memoryGet(id);
        if (!m) return err(`memory not found: ${id}`);
        return result(formatMemory(m), m);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-neighbors",
    description:
      "Walk the memory graph from a given id, returning each neighbor " +
      "with its hop distance and a short preview. Useful before " +
      "memory_link to see what already exists.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string" },
        max_hops: { type: "number", description: "Max hop distance (1-5). Default 2." },
      },
      required: ["id"],
    },
    execute: async (args) => {
      const id = args.id as string | undefined;
      if (!id) return err("memory_neighbors requires id");
      const maxHops = typeof args.max_hops === "number" ? args.max_hops : 2;
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_neighbors failed", async () => {
        const hits = await c.memoryNeighbors(id, maxHops);
        if (hits.length === 0) return result("(no neighbors)", []);
        const lines = hits.map(
          (h) => `[hop ${h.hop}] #${h.memory.id.slice(0, 8)}  ${h.memory.title}\n     ${h.memory.content.slice(0, 100)}`,
        );
        return result(`${hits.length} neighbor(s):\n${lines.join("\n")}`, hits);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-reflect",
    description:
      "Surface recent, under-connected memories for LLM-driven " +
      "reflection. Returns each memory with id/title/category; the " +
      "LLM decides which conceptual links the auto-link layer missed " +
      "and calls memory_link to add them. On-demand only; does not run " +
      "automatically.",
    parameters: {
      type: "object",
      properties: {
        sinceDays: { type: "number", description: "Default 7." },
        limit: { type: "number", description: "Default 20." },
      },
    },
    execute: async (args) => {
      const opts: { sinceDays?: number; limit?: number } = {};
      if (typeof args.sinceDays === "number") opts.sinceDays = args.sinceDays;
      if (typeof args.limit === "number") opts.limit = args.limit;
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_reflect failed", async () => {
        const mems = await c.memoryReflect(opts);
        if (mems.length === 0) return result("(no candidates)", []);
        const lines = mems.map(
          (m) => `- #${m.id.slice(0, 8)}  [${m.category}|imp=${m.importance.toFixed(2)}]  ${m.title}`,
        );
        return result(`${mems.length} candidate(s):\n${lines.join("\n")}`, mems);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-save-search-result",
    description:
      "Explicitly save one or more search hits as memories. Pass the " +
      "memory ids returned by a prior mnemush-memory-search call; each " +
      "becomes a memory with the original content and a 'saved from " +
      "search: <query>' context line. EXPLICIT only — no auto-save.",
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
    execute: async (args) => {
      const ids = args.ids as string[] | undefined;
      const query = args.query as string | undefined;
      if (!ids || !Array.isArray(ids) || ids.length === 0) {
        return err("ids must be a non-empty array");
      }
      if (!query) return err("save_search_result requires query");
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_save_search_result failed", async () => {
        const r = await c.memorySaveSearchResult({
          ids,
          query,
          category: (args.category as never) ?? "note",
          importance: typeof args.importance === "number" ? args.importance : 0.5,
        });
        return result(
          `✓ saved ${r.saved.length}, errors ${r.errors.length}`,
          r,
        );
      });
    },
  });

  registerTool({
    name: "mnemush-status",
    description:
      "One-line summary of memory system state: active memories, " +
      "soft-deleted memories, edges, needs_review, prune candidates, " +
      "reflect candidates, pending identity proposals. Use this when " +
      "you want to know the overall state without running multiple " +
      "commands.",
    parameters: { type: "object", properties: {} },
    execute: async () => {
      await ensureConnected();
      const c = getClient();
      return tryRun("mnemush_status failed", async () => {
        const s = await c.mnemushStatus();
        const text =
          `active=${s.active}  soft_deleted=${s.soft_deleted}  edges=${s.edges}  ` +
          `needs_review=${s.needs_review}  prune_candidates=${s.prune_candidates}  ` +
          `reflect_candidates=${s.reflect_candidates}  pending_proposals=${s.pending_proposals}`;
        return result(text, s);
      });
    },
  });

  registerTool({
    name: "identity-propose",
    description:
      "Propose an update to one of the identity files (USER.md / " +
      "PERSONA.md / CONSTITUTION.md). The proposal is queued for the " +
      "user to review. Use this when you have a high-confidence " +
      "observation about the user that the current identity files " +
      "don't yet capture. Provide a clear reason and an evidenceCount. " +
      "NEVER write to the identity files directly — always go through " +
      "this tool.",
    parameters: {
      type: "object",
      properties: {
        target: {
          type: "string",
          enum: ["USER.md", "PERSONA.md", "CONSTITUTION.md"],
        },
        content: { type: "string" },
        reason: { type: "string" },
        evidenceCount: { type: "number", default: 1 },
      },
      required: ["target", "content", "reason"],
    },
    execute: async (args) => {
      const target = args.target as "USER.md" | "PERSONA.md" | "CONSTITUTION.md" | undefined;
      const content = args.content as string | undefined;
      const reason = args.reason as string | undefined;
      if (!target || !content || !reason) {
        return err("identity-propose requires target, content, and reason");
      }
      await ensureConnected();
      const c = getClient();
      return tryRun("identity_propose failed", async () => {
        const p = await c.identityPropose({
          target, content, reason,
          evidenceCount: typeof args.evidenceCount === "number" ? args.evidenceCount : 1,
        });
        return result(
          `✓ proposal id=${p.id} [${p.status}] → ${p.target}`,
          p,
        );
      });
    },
  });

  registerTool({
    name: "identity-list-pending",
    description:
      "List identity-update proposals so the user (or you, when " +
      "instructed) can review them. Returns id, target, content, " +
      "reason, and evidenceCount for each.",
    parameters: {
      type: "object",
      properties: {
        status: {
          type: "string",
          enum: ["pending", "approved", "rejected"],
        },
        all: { type: "boolean", default: false, description: "If true, return all statuses." },
      },
    },
    execute: async (args) => {
      const opts: { status?: "pending" | "approved" | "rejected"; all?: boolean } = {};
      if (args.status) opts.status = args.status as "pending" | "approved" | "rejected";
      if (args.all) opts.all = true;
      await ensureConnected();
      const c = getClient();
      return tryRun("identity_list_pending failed", async () => {
        const list = await c.identityListPending(opts);
        if (list.length === 0) return result("(no proposals)", []);
        const lines = list.map(
          (p) =>
            `- #${p.id.slice(0, 8)}  [${p.status}|ev=${p.evidence_count}]  → ${p.target}\n  ${p.content}\n  reason: ${p.reason}`,
        );
        return result(`${list.length} proposal(s):\n${lines.join("\n")}`, list);
      });
    },
  });

  registerTool({
    name: "identity-approve",
    description:
      "Approve a pending identity-update proposal. The proposal's " +
      "content is appended to the target file with a dated header; the " +
      "original is preserved. Use the full id from identity-list-pending.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the proposal to approve" },
      },
      required: ["id"],
    },
    execute: async (args) => {
      const id = args.id as string | undefined;
      if (!id) return err("identity-approve requires id");
      await ensureConnected();
      const c = getClient();
      return tryRun("identity_approve failed", async () => {
        const p = await c.identityApprove(id);
        if (!p) return err(`proposal ${id} not found or already resolved`);
        return result(`✓ approved #${p.id.slice(0, 8)} → ${p.target}\n  ${p.content}`, p);
      });
    },
  });

  registerTool({
    name: "identity-reject",
    description:
      "Reject a pending identity-update proposal. The proposal is " +
      "marked rejected with no file change. Use this when the proposal " +
      "is wrong, premature, or duplicative. Use the full id from " +
      "identity-list-pending.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Full UUID of the proposal to reject" },
      },
      required: ["id"],
    },
    execute: async (args) => {
      const id = args.id as string | undefined;
      if (!id) return err("identity-reject requires id");
      await ensureConnected();
      const c = getClient();
      return tryRun("identity_reject failed", async () => {
        const p = await c.identityReject(id);
        if (!p) return err(`proposal ${id} not found or already resolved`);
        return result(`✓ rejected #${p.id.slice(0, 8)} (${p.target})`, p);
      });
    },
  });

  // ── v0.3 agent self-memory (commitments / actions) ────────────────
  // OpenCode namespacing: tools use `mnemush-memory-*` prefix; the unprefixed
  // variants are registered in the Pi extension. Mirror the Pi set so
  // both surfaces can drive v0.3 commitments.

  registerTool({
    name: "mnemush-memory-next",
    description:
      "Return the highest-priority active commitment (status=active, " +
      "category != 'identity'), or null if none exist. Priority order: " +
      "due_at ASC (nulls last), then created_at DESC, then id DESC for " +
      "stable tie-break.",
    parameters: { type: "object", properties: {} },
    execute: async () => {
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_next failed", async () => {
        const m = await c.memoryNext();
        if (!m) return result("(no active commitments)", null);
        return result(`#${m.id.slice(0, 8)}  ${m.title}`, m);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-frontier",
    description:
      "Return all active commitments (status=active, category != 'identity'), " +
      "ordered by due_at ASC (nulls last), then created_at DESC.",
    parameters: { type: "object", properties: {} },
    execute: async () => {
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_frontier failed", async () => {
        const mems = await c.memoryFrontier();
        if (mems.length === 0) return result("(no active commitments)", []);
        const lines = mems.map((m) =>
          `- #${m.id.slice(0, 8)}  imp=${m.importance.toFixed(2)}  ${m.due_at ? `due=${m.due_at}  ` : ""}${m.title}`,
        );
        return result(`${mems.length} active:\n${lines.join("\n")}`, mems);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-action-create",
    description:
      "Create a commitment (work the agent owes). Pass due_at as a " +
      "unix timestamp in seconds for time-bound work. Returns the " +
      "created Memory with its id.",
    parameters: {
      type: "object",
      properties: {
        title: { type: "string" },
        content: { type: "string" },
        importance: { type: "number", default: 0.5 },
        due_at: { type: "number", description: "Unix timestamp (seconds)." },
        claimed_by: { type: "string", description: "Session or owner claiming the work." },
        parent_id: { type: "string", description: "Parent commitment id." },
        tags: { type: "array", items: { type: "string" } },
      },
      required: ["title", "content"],
    },
    execute: async (args) => {
      const title = args.title as string | undefined;
      const content = args.content as string | undefined;
      if (!title || !content) return err("memory-action-create requires title and content");
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_action_create failed", async () => {
        const m = await c.memoryActionCreate({
          title,
          content,
          importance: args.importance as number | undefined,
          due_at: args.due_at as number | undefined,
          claimed_by: args.claimed_by as string | undefined,
          parent_id: args.parent_id as string | undefined,
          tags: args.tags as string[] | undefined,
        });
        return result(`✓ created #${m.id.slice(0, 8)}  ${m.title}`, m);
      });
    },
  });

  registerTool({
    name: "mnemush-memory-action-update",
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
    execute: async (args) => {
      const id = args.id as string | undefined;
      if (!id) return err("memory-action-update requires id");
      await ensureConnected();
      const c = getClient();
      return tryRun("memory_action_update failed", async () => {
        const m = await c.memoryActionUpdate({
          id,
          status: args.status as "active" | "completed" | "abandoned" | undefined,
          due_at: args.due_at as number | null | undefined,
          claimed_by: args.claimed_by as string | null | undefined,
          importance: args.importance as number | undefined,
        });
        const completedTag = m.completed_at ? `  completed_at=${m.completed_at}` : "";
        return result(`✓ #${m.id.slice(0, 8)}  status=${m.status}${completedTag}`, m);
      });
    },
  });
};

export default plugin;
