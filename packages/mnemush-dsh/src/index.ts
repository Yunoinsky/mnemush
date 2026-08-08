// Copyright (c) 2026 Yunoinsky Chen
// Licensed under Mulan Permissive Software License, Version 2 (Mulan PSL v2).

/**
 * mnemush-dsh — DeepSeek Harness (DSH) plugin for mnemush memory.
 *
 * A native Cordis plugin (same contract as `@deepseek-ai/dsh-tool-bash` and
 * `@deepseek-ai/dsh-mcp-client`): it registers the full mnemush memory toolset
 * on `ctx.tools`, injects the concept table (context-priming index) into the
 * system prompt, and runs the same session-end maintenance pass as the Pi and
 * OpenCode adapters.
 *
 * Like mnemush-pi, this plugin deliberately duck-types the DSH service surface
 * instead of importing `@deepseek-ai/*` types, so the package builds in
 * isolation. At runtime DSH supplies the Cordis context; the `mnemush-mcp` /
 * `mnemush` binaries are spawned through the shared `mnemush-client` library
 * (or the CLI, for concept priming and maintenance).
 *
 * Install (into a DSH profile):
 *   dsh plugin --profile <name> add mnemush-dsh
 * Then add a row to the profile's `cordis.patch.yml`:
 *   - id: mnemush
 *     name: mnemush-dsh
 *     config:
 *       conceptLimit: 40
 */

import {
  MnemushClient,
  formatMemory,
  formatSearchHit,
  loadConceptInject,
  runSessionEndMaintenance,
} from "mnemush-client";

// ── Minimal DSH service surface (duck-typed) ────────────────────────

/** Shape of `ctx.tools.register(definition)` accepted by `@deepseek-ai/dsh-tools`. */
interface ToolDefinition {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  output: {
    schema: Record<string, unknown>;
    render: (
      args: Record<string, unknown>,
      value: unknown,
    ) => Array<{ type: "text"; text: string }>;
  };
  execute: (
    args: Record<string, unknown>,
    exec: { signal: AbortSignal },
  ) => Promise<unknown>;
}

interface DshLogger {
  info(message: string): void;
  warn(message: string): void;
  error(message: string): void;
}

/** The subset of the Cordis context this plugin consumes. */
interface DshContext {
  tools: {
    register(definition: ToolDefinition): () => void;
  };
  systemPrompt: {
    section(section: { name: string; order: number; text: string }): () => void;
  };
  effect(setup: () => void | (() => void), label?: string): void;
  on(event: string, callback: (...args: unknown[]) => void | Promise<void>): () => void;
  logger: DshLogger;
}

/** Plugin config, normalized with defaults in {@link apply}. */
export interface MnemushDshConfig {
  /** Absolute path to the `mnemush-mcp` binary; empty = auto-detect. */
  binaryPath?: string;
  /** Custom data dir (overrides `~/.mnemush`). */
  dataDir?: string;
  /** Max concepts in the injected memory index (default 40). */
  conceptLimit?: number;
  /** Inject the concept table into the system prompt (default true). */
  injectConceptTable?: boolean;
  /** Run prune/edge-decay/needs-review/eval-prune on session disposal (default true). */
  maintenanceOnSessionEnd?: boolean;
}

/** Cordis plugin name used by loader diagnostics. */
export const name = "mnemush";

/** Services required before this plugin activates. */
export const inject = ["tools", "systemPrompt"];

// ── Argument helpers (raw-registered tools own their validation) ─────

function asString(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

function asBoolean(v: unknown): boolean {
  return v === true;
}

function asStringArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

function toError(e: unknown): Error {
  return e instanceof Error ? e : new Error(String(e));
}

// ── Plugin ───────────────────────────────────────────────────────────

export async function apply(ctx: DshContext, rawConfig: MnemushDshConfig = {}): Promise<void> {
  const config: Required<MnemushDshConfig> = {
    binaryPath: rawConfig.binaryPath ?? "",
    dataDir: rawConfig.dataDir ?? "",
    conceptLimit: rawConfig.conceptLimit ?? 40,
    injectConceptTable: rawConfig.injectConceptTable ?? true,
    maintenanceOnSessionEnd: rawConfig.maintenanceOnSessionEnd ?? true,
  };

  // 所有工具都以格式化文本为规范输出值。
  const textOutput: ToolDefinition["output"] = {
    schema: { type: "string" },
    render: (_args, value) => [{ type: "text", text: String(value) }],
  };

  // ── MCP client management (process-lifetime, lazily connected) ────
  let client: MnemushClient | null = null;
  let connecting: Promise<MnemushClient> | null = null;

  async function ensureClient(): Promise<MnemushClient> {
    if (client) return client;
    if (!connecting) {
      connecting = MnemushClient.connect({
        binaryPath: config.binaryPath || undefined,
        dataDir: config.dataDir || undefined,
        onLog: (msg) => ctx.logger.warn(`[mnemush] ${msg}`),
      })
        .then((c) => {
          client = c;
          connecting = null;
          return c;
        })
        .catch((e) => {
          connecting = null;
          throw e;
        });
    }
    return connecting;
  }

  // ── Concept table injection + refresh ──────────────────────────────
  let conceptDisposer: (() => void) | null = null;
  // Serialize refreshes: memory writes and session creation can trigger
  // concurrent refreshes, and `systemPrompt.section` rejects duplicate names
  // in one layer, so the dispose-then-register swap must be atomic.
  let refreshChain: Promise<void> = Promise.resolve();

  function refreshConceptSection(): Promise<void> {
    if (!config.injectConceptTable) return Promise.resolve();
    const run = refreshChain.then(async () => {
      const text = await loadConceptInject(config.conceptLimit, config.dataDir || undefined).catch(
        () => null,
      );
      try {
        conceptDisposer?.();
        conceptDisposer = text
          ? ctx.systemPrompt.section({
              name: "mnemush:memory-index",
              order: 90,
              text,
            })
          : null;
      } catch (e) {
        conceptDisposer = null;
        ctx.logger.warn(`[mnemush] concept-table injection failed: ${toError(e).message}`);
      }
    });
    refreshChain = run.catch(() => {});
    return refreshChain;
  }

  /** Wrap a tool body: abort checks + error normalization. */
  function run(exec: { signal: AbortSignal }, fn: () => Promise<unknown>): Promise<unknown> {
    if (exec.signal.aborted) return Promise.reject(new Error("tool call aborted"));
    return fn()
      .then((out) => {
        if (exec.signal.aborted) throw new Error("tool call aborted");
        return out;
      })
      .catch((e) => {
        throw toError(e);
      });
  }

  /** Successful memory writes invalidate the injected index. */
  function bumpConceptSection(): void {
    if (config.injectConceptTable) void refreshConceptSection();
  }

  // ── Tools ──────────────────────────────────────────────────────────

  ctx.tools.register({
    name: "memory_add",
    description:
      "Add a new memory. Returns id and any conflict candidates (existing " +
      "memories the new one was auto-linked to).",
    parameters: {
      type: "object",
      properties: {
        content: { type: "string", description: "The memory body." },
        title: { type: "string", description: "Short title." },
        category: {
          type: "string",
          enum: ["decision", "lesson", "failure", "correction", "insight", "preference", "convention", "tool_quirk", "episodic", "skill", "identity", "note"],
          default: "note",
        },
        memory_type: {
          type: "string",
          enum: ["semantic", "procedural", "identity"],
          default: "semantic",
        },
        importance: { type: "number", description: "0.0-1.0. Default 0.5." },
        tags: { type: "array", items: { type: "string" } },
        project: { type: "string" },
        context: { type: "string" },
        needs_review: { type: "boolean", default: false },
      },
      required: ["content", "title"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const title = asString(args.title);
        const content = asString(args.content);
        if (!title || !content) throw new Error("memory_add requires both title and content");
        const c = await ensureClient();
        const r = await c.memoryAdd({
          title,
          content,
          category: (args.category as never) ?? "note",
          memory_type: (args.memory_type as never) ?? "semantic",
          importance: asNumber(args.importance) ?? 0.5,
          tags: asStringArray(args.tags),
          project: asString(args.project),
          context: asString(args.context),
          needs_review: asBoolean(args.needs_review),
        });
        bumpConceptSection();
        let out = `✓ added #${r.id.slice(0, 8)}`;
        if (r.conflicts.length > 0) {
          out += `\n🔗 linked ${r.conflicts.length} related memory(ies):\n`;
          for (const m of r.conflicts.slice(0, 3)) out += `  - ${m.title} (${m.category})\n`;
        }
        return out;
      }),
  });

  ctx.tools.register({
    name: "memory_search",
    description:
      "Search memories via FTS5 + confidence scoring. Returns ranked hits with " +
      "id, title, content, category, and score.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Search query." },
        category: { type: "string", description: "Category filter." },
        project: { type: "string", description: "Project filter." },
        limit: { type: "integer", default: 10 },
      },
      required: ["query"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const query = asString(args.query);
        if (!query) throw new Error("memory_search requires query");
        const c = await ensureClient();
        const hits = await c.memorySearch(query, {
          category: (args.category as never) ?? undefined,
          project: asString(args.project),
          limit: asNumber(args.limit) ?? 10,
        });
        if (hits.length === 0) return "(no matches)";
        return hits.map(formatSearchHit).join("\n\n");
      }),
  });

  ctx.tools.register({
    name: "memory_get",
    description:
      "Get a single memory by its full UUID. Search hits only expose an 8-char " +
      "prefix; use this to retrieve the full id and metadata before linking.",
    parameters: {
      type: "object",
      properties: { id: { type: "string", description: "Full UUID." } },
      required: ["id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const id = asString(args.id);
        if (!id) throw new Error("memory_get requires id");
        const c = await ensureClient();
        const m = await c.memoryGet(id);
        if (!m) throw new Error(`memory not found: ${id}`);
        return formatMemory(m);
      }),
  });

  ctx.tools.register({
    name: "memory_link",
    description:
      "Create or strengthen an edge between two memories (related, supports, " +
      "contradicts, supersedes).",
    parameters: {
      type: "object",
      properties: {
        source_id: { type: "string" },
        target_id: { type: "string" },
        edge_type: { type: "string", enum: ["related", "supports", "contradicts", "supersedes"], default: "related" },
        strength: { type: "number", default: 0.5 },
      },
      required: ["source_id", "target_id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const sourceId = asString(args.source_id);
        const targetId = asString(args.target_id);
        if (!sourceId || !targetId) throw new Error("memory_link requires source_id and target_id");
        const c = await ensureClient();
        const edge = await c.memoryLink(
          sourceId,
          targetId,
          (args.edge_type as never) ?? "related",
          asNumber(args.strength) ?? 0.5,
        );
        return `✓ linked #${edge.id.slice(0, 8)} (${edge.edge_type}, strength=${edge.strength})`;
      }),
  });

  ctx.tools.register({
    name: "memory_neighbors",
    description:
      "Walk the memory graph from a given id, returning each neighbor with its " +
      "hop distance (1..max_hops) and a short preview.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string" },
        max_hops: { type: "integer", default: 2 },
      },
      required: ["id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const id = asString(args.id);
        if (!id) throw new Error("memory_neighbors requires id");
        const c = await ensureClient();
        const hits = await c.memoryNeighbors(id, asNumber(args.max_hops) ?? 2);
        if (hits.length === 0) return "(no neighbors)";
        const lines = hits.map(
          (h) => `[hop ${h.hop}] #${h.memory.id.slice(0, 8)}  ${h.memory.title}\n     ${h.memory.content.slice(0, 100)}`,
        );
        return `${hits.length} neighbor(s):\n${lines.join("\n")}`;
      }),
  });

  ctx.tools.register({
    name: "memory_reflect",
    description:
      "Surface recent, under-connected memories for LLM-driven reflection. For " +
      "each candidate, decide whether it links to other memories and call " +
      "memory_link to add the missing edge.",
    parameters: {
      type: "object",
      properties: {
        since_days: { type: "integer", default: 7 },
        limit: { type: "integer", default: 20 },
      },
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const c = await ensureClient();
        const mems = await c.memoryReflect({
          sinceDays: asNumber(args.since_days) ?? 7,
          limit: asNumber(args.limit) ?? 20,
        });
        if (mems.length === 0) return "(no candidates)";
        const lines = mems.map(
          (m) => `- #${m.id.slice(0, 8)}  [${m.category}|imp=${m.importance.toFixed(2)}]  ${m.title}\n  ${m.content.slice(0, 120)}`,
        );
        return `${mems.length} candidate(s):\n${lines.join("\n")}`;
      }),
  });

  ctx.tools.register({
    name: "memory_save_search_result",
    description:
      "Explicitly save one or more search hits as memories. Pass the ids from a " +
      "prior memory_search call; each becomes a memory with the original content " +
      "and a 'saved from search: <query>' context line. NEVER auto-save.",
    parameters: {
      type: "object",
      properties: {
        ids: { type: "array", items: { type: "string" } },
        query: { type: "string", description: "The original search query." },
        category: { type: "string", default: "note" },
        importance: { type: "number", default: 0.5 },
      },
      required: ["ids", "query"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const ids = asStringArray(args.ids);
        const query = asString(args.query);
        if (ids.length === 0) throw new Error("memory_save_search_result requires a non-empty ids array");
        if (!query) throw new Error("memory_save_search_result requires query");
        const c = await ensureClient();
        const r = await c.memorySaveSearchResult({
          ids,
          query,
          category: (args.category as never) ?? "note",
          importance: asNumber(args.importance) ?? 0.5,
        });
        bumpConceptSection();
        const n = r.saved?.length ?? 0;
        const errs = r.errors ?? [];
        return errs.length > 0
          ? `saved ${n}, errors: ${errs.join("; ")}`
          : `saved ${n} memory(ies)`;
      }),
  });

  ctx.tools.register({
    name: "mnemush_status",
    description:
      "One-line summary of memory system state: active/soft-deleted counts, " +
      "edges, needs_review, prune candidates, reflect candidates, pending " +
      "identity proposals.",
    parameters: { type: "object", properties: {} },
    output: textOutput,
    execute: (_args, exec) =>
      run(exec, async () => {
        const c = await ensureClient();
        const s = await c.mnemushStatus();
        return (
          `active=${s.active}  soft_deleted=${s.soft_deleted}  edges=${s.edges}  ` +
          `needs_review=${s.needs_review}  prune_candidates=${s.prune_candidates}  ` +
          `reflect_candidates=${s.reflect_candidates}  pending_proposals=${s.pending_proposals}`
        );
      }),
  });

  ctx.tools.register({
    name: "memory_next",
    description:
      "Return the highest-priority active commitment (status=active, category " +
      "!= 'identity'), or null if none exist.",
    parameters: { type: "object", properties: {} },
    output: textOutput,
    execute: (_args, exec) =>
      run(exec, async () => {
        const c = await ensureClient();
        const m = await c.memoryNext();
        if (!m) return "(no active commitments)";
        return `#${m.id.slice(0, 8)}  ${m.title}`;
      }),
  });

  ctx.tools.register({
    name: "memory_frontier",
    description: "List all active commitments, ordered by priority.",
    parameters: {
      type: "object",
      properties: { limit: { type: "number", default: 20 } },
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const c = await ensureClient();
        const mems = await c.memoryFrontier();
        if (mems.length === 0) return "(no active commitments)";
        const limit = asNumber(args.limit) ?? 20;
        const lines = mems
          .slice(0, limit)
          .map((m) => `- #${m.id.slice(0, 8)}  imp=${m.importance.toFixed(2)}  ${m.due_at ? `due=${m.due_at}  ` : ""}${m.title}`);
        return `${Math.min(mems.length, limit)} active:\n${lines.join("\n")}`;
      }),
  });

  ctx.tools.register({
    name: "memory_action_create",
    description:
      "Create a commitment (work the agent owes). Pass due_at as a unix " +
      "timestamp in seconds for time-bound work.",
    parameters: {
      type: "object",
      properties: {
        title: { type: "string" },
        content: { type: "string" },
        importance: { type: "number", default: 0.7 },
        due_at: { type: "number", description: "Unix seconds deadline." },
        claimed_by: { type: "string", description: "Agent id claiming this action." },
        parent_id: { type: "string", description: "Parent action id." },
        tags: { type: "array", items: { type: "string" } },
      },
      required: ["title", "content"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const title = asString(args.title);
        const content = asString(args.content);
        if (!title || !content) throw new Error("memory_action_create requires title and content");
        const c = await ensureClient();
        const m = await c.memoryActionCreate({
          title,
          content,
          importance: asNumber(args.importance) ?? 0.7,
          due_at: asNumber(args.due_at),
          claimed_by: asString(args.claimed_by),
          parent_id: asString(args.parent_id),
          tags: asStringArray(args.tags),
        });
        bumpConceptSection();
        return `✓ created #${m.id.slice(0, 8)}  ${m.title}`;
      }),
  });

  ctx.tools.register({
    name: "memory_action_update",
    description:
      "Update a commitment. On status transition to 'completed'/'abandoned' the " +
      "server auto-sets completed_at; back to 'active' clears it.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string" },
        status: { type: "string", enum: ["active", "completed", "abandoned"] },
        due_at: { type: "number", description: "Unix seconds. Pass null to clear." },
        claimed_by: { type: "string", description: "Pass null to unclaim." },
        importance: { type: "number" },
      },
      required: ["id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const id = asString(args.id);
        if (!id) throw new Error("memory_action_update requires id");
        const c = await ensureClient();
        const m = await c.memoryActionUpdate({
          id,
          status: (args.status as never) ?? undefined,
          due_at: asNumber(args.due_at),
          claimed_by: asString(args.claimed_by),
          importance: asNumber(args.importance),
        });
        bumpConceptSection();
        const completedTag = m.completed_at ? `  completed_at=${m.completed_at}` : "";
        return `✓ #${m.id.slice(0, 8)}  status=${m.status}${completedTag}`;
      }),
  });

  ctx.tools.register({
    name: "identity_propose",
    description:
      "Propose an update to USER.md / PERSONA.md / CONSTITUTION.md. The proposal " +
      "is queued for review; NEVER write to the identity files directly.",
    parameters: {
      type: "object",
      properties: {
        target: { type: "string", enum: ["USER.md", "PERSONA.md", "CONSTITUTION.md"] },
        content: { type: "string" },
        reason: { type: "string" },
        evidence_count: { type: "integer", default: 1 },
      },
      required: ["target", "content", "reason"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const target = asString(args.target);
        const content = asString(args.content);
        const reason = asString(args.reason);
        if (!target || !content || !reason) {
          throw new Error("identity_propose requires target, content, and reason");
        }
        const c = await ensureClient();
        const p = await c.identityPropose({
          target: target as "USER.md" | "PERSONA.md" | "CONSTITUTION.md",
          content,
          reason,
          evidenceCount: asNumber(args.evidence_count) ?? 1,
        });
        return `✓ proposed #${p.id.slice(0, 8)} → ${p.target}\n  reason: ${p.reason}\n  evidence: ${p.evidence_count}`;
      }),
  });

  ctx.tools.register({
    name: "identity_list_pending",
    description:
      "List identity-update proposals. Default filters to pending only; pass " +
      "all=true to see approved/rejected history.",
    parameters: {
      type: "object",
      properties: {
        status: { type: "string", enum: ["pending", "approved", "rejected"] },
        all: { type: "boolean", default: false },
      },
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const c = await ensureClient();
        const list = await c.identityListPending({
          status: (args.status as never) ?? undefined,
          all: asBoolean(args.all),
        });
        if (list.length === 0) return "(no proposals)";
        const lines = list.map(
          (p) => `- #${p.id.slice(0, 8)}  [${p.status}|ev=${p.evidence_count}]  → ${p.target}\n  ${p.content}\n  reason: ${p.reason}`,
        );
        return `${list.length} proposal(s):\n${lines.join("\n")}`;
      }),
  });

  ctx.tools.register({
    name: "identity_approve",
    description:
      "Approve a pending identity-update proposal. Appends its content to the " +
      "target file as a dated section.",
    parameters: {
      type: "object",
      properties: { id: { type: "string" } },
      required: ["id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const id = asString(args.id);
        if (!id) throw new Error("identity_approve requires id");
        const c = await ensureClient();
        const p = await c.identityApprove(id);
        if (!p) throw new Error(`proposal ${id} not found or already resolved`);
        return `✓ approved #${p.id.slice(0, 8)} → ${p.target}\n  ${p.content}`;
      }),
  });

  ctx.tools.register({
    name: "identity_reject",
    description:
      "Reject a pending identity-update proposal. The target file is NOT touched.",
    parameters: {
      type: "object",
      properties: { id: { type: "string" } },
      required: ["id"],
    },
    output: textOutput,
    execute: (args, exec) =>
      run(exec, async () => {
        const id = asString(args.id);
        if (!id) throw new Error("identity_reject requires id");
        const c = await ensureClient();
        const p = await c.identityReject(id);
        if (!p) throw new Error(`proposal ${id} not found or already resolved`);
        return `✓ rejected #${p.id.slice(0, 8)} (${p.target})`;
      }),
  });

  // ── Lifecycle ──────────────────────────────────────────────────────
  // Warm the concept table for the first turn. Bounded by the CLI timeout,
  // so a slow/missing binary never wedges startup.
  await refreshConceptSection();

  // Re-prime the index on each new session, matching Pi's session_start inject.
  ctx.on("session/created", () => {
    void refreshConceptSection();
  });

  // Session-end maintenance, mirroring the Pi/OpenCode session_end pass.
  ctx.on("session/disposed", () => {
    if (config.maintenanceOnSessionEnd) {
      runSessionEndMaintenance({ dataDir: config.dataDir || undefined }).catch((e) => {
        ctx.logger.warn(`[mnemush] session-end maintenance failed: ${toError(e).message}`);
      });
    }
  });

  // Disconnect the MCP subprocess and release the concept section on dispose.
  ctx.effect(() => {
    return () => {
      conceptDisposer?.();
      conceptDisposer = null;
      void client?.disconnect().catch(() => {});
      client = null;
    };
  }, "mnemush.dispose");
}
