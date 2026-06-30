/**
 * mneme-opencode — OpenCode plugin for mneme memory.
 *
 * Same architecture as the Pi extension. OpenCode's plugin API uses
 * different event names (chat.message, tool.execute.after) so we
 * provide thin shims that adapt to mneme-client.
 *
 * To install globally:
 *   npm install -g mneme-opencode
 * Then symlink dist/index.js into ~/.config/opencode/plugin/ or use the
 * auto-discovery script in scripts/install.sh.
 */

import {
  MnemeClient,
  formatMemory,
  formatSearchHit as formatHit,
  looksLikeCorrection,
  looksLikeRemember,
} from "mneme-client";

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

let client: MnemeClient | null = null;

function getClient(): MnemeClient {
  if (!client) throw new Error("mneme-opencode: not connected");
  return client;
}

function result(text: string): { content: Array<{ type: "text"; text: string }> } {
  return { content: [{ type: "text", text }] };
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
      console.error(`[mneme] auto-capture failed: ${err}`);
    }
  });

  // ── tool.execute.after: capture tool failures ───────────────
  oc.on("tool.execute.after", async (event) => {
    if (!client) return;
    const e = event as { name?: string; result?: { error?: string; is_error?: boolean } } | undefined;
    if (!e || e.name === "mneme-memory" || e.name === "mneme-memory-search") return;
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
        console.error(`[mneme] tool-failure save failed: ${err}`);
      }
    }
  });

  // ── session lifecycle: connect / disconnect ─────────────────
  // OpenCode doesn't expose explicit start/end events; the plugin
  // function itself runs at startup. We connect lazily on first tool
  // call to avoid blocking plugin load.

  async function ensureConnected() {
    if (client) return;
    client = await MnemeClient.connect({
      onLog: (msg) => console.error(`[mneme] ${msg}`),
    });
  }

  // ── Tools ────────────────────────────────────────────────────

  oc.tool({
    name: "mneme-memory",
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
          return result(`✓ added #${r.id.slice(0, 8)}`);
        }
        if (action === "search") {
          if (!args.query) return err("search requires query");
          const hits = await c.memorySearch(args.query as string, {
            limit: (args.limit as number) ?? 10,
          });
          if (hits.length === 0) return result("(no matches)");
          return result(hits.map(formatHit).join("\n\n"));
        }
        return err(`unknown action: ${action}`);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  oc.tool({
    name: "mneme-memory-search",
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
        return result(hits.map(formatHit).join("\n\n"));
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });

  oc.tool({
    name: "mneme-memory-link",
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
      await ensureConnected();
      const c = getClient();
      try {
        const edge = await c.memoryLink(
          args.source_id as string,
          args.target_id as string,
          (args.edge_type as never) ?? "related",
          (args.strength as number) ?? 0.5,
        );
        return result(`✓ linked #${edge.id.slice(0, 8)} (${edge.edge_type})`);
      } catch (e) {
        return err(e instanceof Error ? e.message : String(e));
      }
    },
  });
};

export default plugin;
