/**
 * mneme-pi — Pi extension for mneme memory.
 *
 * Registers:
 *   - Hooks: session_start, session_end, user_prompt_submit, after_tool_call
 *   - Tools: memory (CRUD), memory_search, memory_link
 *
 * The extension auto-spawns the mneme-mcp binary on session start and
 * disconnects on session end. All memory operations are routed through
 * the shared mneme-client library.
 */

// Pi SDK types are inferred via duck-typing. The extension exports a
// default function that accepts the Pi extension API. We intentionally
// avoid a hard dependency on @earendil-works/pi-coding-agent so the
// package stays buildable in isolation.

import {
  MnemeClient,
  formatMemory,
  formatSearchHit,
  looksLikeCorrection,
  looksLikeRemember,
} from "mneme-client";

interface ToolDefinition {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  execute: (args: Record<string, unknown>, ctx: unknown) => Promise<unknown>;
}

interface ToolResult<T> {
  content: Array<{ type: "text"; text: string }>;
  isError?: boolean;
  details?: T;
}

interface ExtensionAPI {
  registerTool: <T>(tool: ToolDefinition & {
    execute: (args: Record<string, unknown>, ctx: unknown) => Promise<ToolResult<T>>;
  }) => void;
  on: (event: string, handler: (event: unknown, ctx: unknown) => void | Promise<void>) => void;
  sendMessage?: (msg: string) => void;
  sendStatus?: (status: string, ttlMs?: number) => void;
}

let client: MnemeClient | null = null;

function getClient(): MnemeClient {
  if (!client) {
    throw new Error("mneme-pi: not connected. Session start hook may have failed.");
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

export default function activate(pi: ExtensionAPI): void {
  // ── session_start: connect to mneme-mcp ─────────────────────
  pi.on("session_start", async () => {
    try {
      client = await MnemeClient.connect({
        onLog: (msg) => console.error(`[mneme] ${msg}`),
      });
      pi.sendStatus?.("🧠 mneme connected", 5000);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error(`[mneme] failed to connect: ${msg}`);
      pi.sendStatus?.(`⚠️ mneme unavailable: ${msg}`, 10000);
    }
  });

  // ── session_end: disconnect ─────────────────────────────────
  pi.on("session_end", async () => {
    if (client) {
      try {
        await client.disconnect();
      } catch (e) {
        console.error(`[mneme] disconnect error: ${e}`);
      }
      client = null;
    }
  });

  // ── user_prompt_submit: heuristic capture ───────────────────
  pi.on("user_prompt_submit", async (event) => {
    if (!client) return;
    const e = event as { text?: string } | undefined;
    const text = e?.text ?? "";
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
      } else if (looksLikeCorrection(text)) {
        await client.memoryAdd({
          title: text.slice(0, 80),
          content: text,
          category: "correction",
          importance: 0.9,
          source: "auto_heuristic" as never,
        });
        pi.sendStatus?.("🧠 saved (correction)", 3000);
      }
    } catch (e) {
      console.error(`[mneme] auto-capture failed: ${e}`);
    }
  });

  // ── after_tool_call: heuristic capture for tool errors ───────
  pi.on("after_tool_call", async (event) => {
    if (!client) return;
    const e = event as { tool_name?: string; result?: { error?: string; is_error?: boolean } } | undefined;
    if (!e || e.tool_name === "mneme-memory" || e.tool_name === "mneme-memory_search") return;
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
        console.error(`[mneme] tool-failure save failed: ${err}`);
      }
    }
  });

  // ── Tools ────────────────────────────────────────────────────

  pi.registerTool({
    name: "memory",
    description:
      "Persistent memory: add / search / replace / remove. " +
      "Action is required. Use this to save decisions, preferences, " +
      "conventions, or anything worth remembering across sessions. " +
      "Also use it to retrieve prior knowledge before answering.",
    parameters: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["add", "search", "replace", "remove"],
          description: "What to do with the memory.",
        },
        content: { type: "string", description: "Memory content (for add/replace)." },
        title: { type: "string", description: "Short title (for add)." },
        id: { type: "string", description: "Memory id (for replace/remove/search by id)." },
        category: {
          type: "string",
          enum: ["decision", "lesson", "failure", "correction", "insight", "preference", "convention", "tool_quirk", "episodic", "skill", "identity", "note"],
          description: "Category (for add).",
        },
        importance: { type: "number", description: "0.0-1.0 (for add). Defaults to 0.5." },
        query: { type: "string", description: "Search query (for search)." },
        limit: { type: "number", description: "Max results (for search). Default 10." },
      },
      required: ["action"],
    },
    execute: async (args) => {
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
              out += `\n⚠ ${r.conflicts.length} conflict(s):\n`;
              for (const c2 of r.conflicts.slice(0, 3)) {
                out += `  - ${c2.title} (${c2.category})\n`;
              }
            }
            return result(out, r);
          }
          case "search": {
            if (!args.query) return err("search requires query");
            const hits = await c.memorySearch(args.query as string, {
              limit: (args.limit as number) ?? 10,
            });
            if (hits.length === 0) return result("(no matches)");
            return result(hits.map(formatSearchHit).join("\n\n"), hits);
          }
          case "replace": {
            // For simplicity: get existing, update content, re-save is not
            // supported in the v0.1 MCP surface. Recommend delete + add.
            return err(
              "replace is not yet supported. Use action=remove then action=add to rewrite a memory.",
            );
          }
          case "remove": {
            if (!args.id) return err("remove requires id");
            // Soft-delete not yet exposed via MCP; we recommend add a
            // 'supersedes' edge or just leave the memory and add a
            // correction with the new content.
            return err(
              "remove is not yet exposed via the v0.1 MCP surface. " +
                "Use memory_search to find the old memory and add a new " +
                "one with category=correction, then link with edge_type=supersedes.",
            );
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
    name: "memory_search",
    description:
      "Advanced search of long-term memory. Returns ranked hits with " +
      "memory body and metadata. Use this when you need context that " +
      "isn't in the current session.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string" },
        category: { type: "string" },
        project: { type: "string" },
        limit: { type: "number" },
      },
      required: ["query"],
    },
    execute: async (args) => {
      const c = getClient();
      try {
        const hits = await c.memorySearch(args.query as string, {
          category: args.category as never,
          project: args.project as string | undefined,
          limit: (args.limit as number) ?? 10,
        });
        if (hits.length === 0) return result("(no matches)");
        return result(hits.map(formatSearchHit).join("\n\n"), hits);
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
    execute: async (args) => {
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
}
