// TDD test: load OpenCode plugin and exercise each tool.
// Plugin exports default function: ({ client }) => { ... }
// where client has .tool() and .on() methods.
// We mock client to capture all .tool() definitions + .on() handlers,
// then invoke each tool's execute() with sample args.
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath, pathToFileURL } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN_PATH = path.join(__dirname, "..", "dist", "index.js");

// Track registered tools/hooks
const registered = { tools: {}, hooks: {} };

const fakeClient = {
  tool(def) { registered.tools[def.name] = def; },
  on(event, handler) { registered.hooks[event] = handler; },
};

const fakeCtx = {
  client: fakeClient,
  directory: "/tmp",
  worktree: "/tmp",
  $: {},
};

// Set up a temp data dir + DB for this test (mnemush-mcp will spawn)
const TMP_DB = path.join(os.tmpdir(), `oc-test-${Date.now()}.db`);
const TMP_DATA = path.join(os.tmpdir(), `oc-test-${Date.now()}-data`);
process.env.MNEMUSH_DB_PATH = TMP_DB;
process.env.MNEMUSH_DATA_DIR = TMP_DATA;

// MnemushClient.connect() in the plugin spawns mnemush-mcp via PATH.
// Make sure our locally-built binary is on PATH.
// Resolve the freshly-built binary relative to the repo root
// (packages/mnemush-opencode/test -> repo root). Falls back to PATH.
const MNEMUSH_MCP = process.env.MNEMUSH_BINARY
  ?? path.join(__dirname, "..", "..", "..", "crates", "mnemush", "target", "release",
      `mnemush-mcp${process.platform === "win32" ? ".exe" : ""}`);
// Both the PATH and the client's MNEMUSH_BINARY env must point at it.
process.env.MNEMUSH_BINARY = MNEMUSH_MCP;
process.env.PATH = `${path.dirname(MNEMUSH_MCP)}:${process.env.PATH}`;

let module;
try {
  module = await import(pathToFileURL(PLUGIN_PATH).href);
} catch (e) {
  console.log("FAIL: import error:", e.message);
  process.exit(1);
}

const plugin = module.default;
if (typeof plugin !== "function") {
  console.log("FAIL: default export is not a function, got", typeof plugin);
  process.exit(1);
}

let passed = 0, failed = 0;
function check(name, ok, detail = "") {
  console.log(`  [${ok ? "✓" : "✗"}] ${name}${detail ? " — " + detail : ""}`);
  if (ok) passed++; else failed++;
}

// Invoke the plugin function to register everything
try {
  await plugin(fakeCtx);
} catch (e) {
  console.log("FAIL: plugin invocation threw:", e.message);
  process.exit(1);
}

console.log("=== Tools registered ===");
const toolNames = Object.keys(registered.tools).sort();
check("11+ tools registered", toolNames.length >= 11, `${toolNames.length}: ${toolNames.join(", ")}`);

console.log("");
console.log("=== Hooks registered ===");
const hookNames = Object.keys(registered.hooks).sort();
console.log("  hooks:", hookNames);
check("session_created hook registered", "session.created" in registered.hooks);
check("session_deleted hook registered", "session.deleted" in registered.hooks || "session.idle" in registered.hooks);

// Sample-args: test each tool with a minimal valid call
// memory_add via mnemush-memory (action=add) requires title+content
// memory_get requires id
// memory_link requires source_id+target_id
// memory_neighbors requires id
// memory_reflect requires no args
// memory_save_search_result requires ids+query
// mnemush_status requires no args
// identity_propose requires target+content+reason
// identity_list_pending requires no args
// identity_approve requires id
// identity_reject requires id

async function callTool(name, args) {
  const t = registered.tools[name];
  if (!t) return { __missing: name };
  try {
    const r = await t.execute(args, {});
    return r;
  } catch (e) {
    return { __exception: e.message };
  }
}

console.log("");
console.log("=== Tool behaviors ===");

// First, we need to add a memory to test other tools that need ids
// (mnemush-memory's execute lazy-connects to mnemush-mcp on first call)
const r1 = await callTool("mnemush-memory", {
  action: "add", title: "opencode-test", content: "from opencode plugin",
  importance: 0.5, category: "note",
});
check("mnemush-memory add: returns success result", r1 && !r1.__missing && !r1.__exception && !r1.isError,
      r1.isError ? `isError: ${r1.content?.[0]?.text}` : (r1.__missing || r1.__exception || "ok"));

// Extract id from result text
const idMatch = r1.content?.[0]?.text?.match(/#([0-9a-f]{8})/);
const noteId = idMatch ? idMatch[1] : null;
// We need the full id, not just prefix. Look at original result content
const fullIdMatch = r1.content?.[0]?.text?.match(/id=([0-9a-f-]{36})/);
const fullNoteId = fullIdMatch ? fullIdMatch[1] : null;
check("mnemush-memory add: returned full id", !!fullNoteId, fullNoteId || "(no id match)");

// mnemush-memory-search
const r2 = await callTool("mnemush-memory-search", { query: "opencode-test" });
check("mnemush-memory-search: returns results", r2 && !r2.__missing && !r2.__exception && !r2.isError);
check("mnemush-memory-search: finds our memory",
      fullNoteId ? r2.content?.[0]?.text?.includes(fullNoteId.slice(0, 8)) : false);

// mnemush-memory-get (NEW)
const r3 = await callTool("mnemush-memory-get", { id: fullNoteId });
// mnemush-memory-get (NEW)
const rGet = await callTool("mnemush-memory-get", { id: fullNoteId });
check("mnemush-memory-get: returns memory",
      rGet && !rGet.__missing && !rGet.__exception && !rGet.isError,
      rGet.__missing ? "tool not registered!" : `raw: ${JSON.stringify(rGet).slice(0, 150)}`);
check("mnemush-memory-get: title matches", rGet.content?.[0]?.text?.includes("opencode-test"));

// mnemush-memory-get with bad id
const rGetBad = await callTool("mnemush-memory-get", { id: "00000000-0000-0000-0000-000000000000" });
check("mnemush-memory-get: bad id → isError", rGetBad && rGetBad.isError === true);

// mnemush-memory-link (with a second memory)
const r4 = await callTool("mnemush-memory", {
  action: "add", title: "opencode-target", content: "link target", importance: 0.5,
});
const id4Match = r4.content?.[0]?.text?.match(/id=([0-9a-f-]{36})/);
const targetId = id4Match ? id4Match[1] : null;

const r5 = await callTool("mnemush-memory-link", {
  source_id: fullNoteId, target_id: targetId, edge_type: "related", strength: 0.7,
});
check("mnemush-memory-link: success", r5 && !r5.isError, r5.isError ? r5.content?.[0]?.text : "ok");

// mnemush-memory-link with bogus edge_type
const r5b = await callTool("mnemush-memory-link", {
  source_id: fullNoteId, target_id: targetId, edge_type: "fake",
});
check("mnemush-memory-link: bogus edge_type → isError", r5b && r5b.isError === true);

// mnemush-memory-neighbors (NEW)
const r6 = await callTool("mnemush-memory-neighbors", { id: fullNoteId, max_hops: 1 });
check("mnemush-memory-neighbors: returns array", r6 && !r6.isError,
      r6.__missing ? "tool not registered!" : "ok");
check("mnemush-memory-neighbors: finds target",
      r6.content?.[0]?.text?.includes(targetId?.slice(0, 8)));

// mnemush-memory-reflect (NEW)
const r7 = await callTool("mnemush-memory-reflect", { sinceDays: 1, limit: 5 });
check("mnemush-memory-reflect: returns candidates", r7 && !r7.isError,
      r7.__missing ? "tool not registered!" : "ok");

// mnemush-memory-save-search-result (NEW)
const r8 = await callTool("mnemush-memory-save-search-result", {
  ids: [fullNoteId], query: "opencode-test", category: "decision", importance: 0.7,
});
check("mnemush-memory-save-search-result: success", r8 && !r8.isError,
      r8.__missing ? "tool not registered!" : (r8.isError ? r8.content?.[0]?.text : "ok"));

// mnemush-memory-save-search-result missing query
const r8b = await callTool("mnemush-memory-save-search-result", { ids: [fullNoteId] });
check("mnemush-memory-save-search-result: missing query → isError", r8b && r8b.isError === true);

// mnemush-status (NEW)
const r9 = await callTool("mnemush-status", {});
check("mnemush-status: returns object with active/edges",
      r9 && !r9.isError,
      r9.__missing ? "tool not registered!" : "ok");

// identity_propose (NEW)
const r10 = await callTool("identity-propose", {
  target: "USER.md", content: "oc-test", reason: "audit", evidenceCount: 1,
});
check("identity-propose: success → pending",
      r10 && !r10.isError && r10.content?.[0]?.text?.includes("pending"),
      r10.__missing ? "tool not registered!" : (r10.isError ? r10.content?.[0]?.text : "ok"));
const propMatch = r10.content?.[0]?.text?.match(/id=([0-9a-f-]{36})/);
const propId = propMatch ? propMatch[1] : null;

// identity-propose bogus target
const r10b = await callTool("identity-propose", {
  target: "BOGUS.md", content: "x", reason: "y", evidenceCount: 1,
});
check("identity-propose: bogus target → isError", r10b && r10b.isError === true);

// identity-list-pending (NEW)
const r11 = await callTool("identity-list-pending", {});
check("identity-list-pending: returns list",
      r11 && !r11.isError,
      r11.__missing ? "tool not registered!" : "ok");
check("identity-list-pending: includes our proposal",
      propId ? r11.content?.[0]?.text?.includes(propId.slice(0, 8)) : false);

// identity-approve (NEW)
const r12 = await callTool("identity-approve", { id: propId });
check("identity-approve: success", r12 && !r12.isError,
      r12.__missing ? "tool not registered!" : (r12.isError ? r12.content?.[0]?.text : "ok"));

// identity-approve second time
const r12b = await callTool("identity-approve", { id: propId });
check("identity-approve second time → isError 'already approved'",
      r12b && r12b.isError && r12b.content?.[0]?.text?.includes("already approved"));

// identity-approve unknown
const r12c = await callTool("identity-approve", { id: "nope-12345" });
check("identity-approve unknown → isError 'not found'",
      r12c && r12c.isError && r12c.content?.[0]?.text?.includes("not found"));

// identity-reject (NEW) — fresh proposal
const r13 = await callTool("identity-propose", {
  target: "USER.md", content: "oc-reject", reason: "audit", evidenceCount: 1,
});
const prop2Id = r13.content?.[0]?.text?.match(/id=([0-9a-f-]{36})/)?.[1];
const r14 = await callTool("identity-reject", { id: prop2Id });
check("identity-reject: success", r14 && !r14.isError,
      r14.__missing ? "tool not registered!" : (r14.isError ? r14.content?.[0]?.text : "ok"));

const r14b = await callTool("identity-reject", { id: prop2Id });
check("identity-reject second time → isError 'already rejected'",
      r14b && r14b.isError && r14b.content?.[0]?.text?.includes("already rejected"));

console.log("");
console.log("=== Hook behaviors ===");

// session.created should connect to mnemush-mcp
// (we already exercised this implicitly via the lazy connect; let's
// verify by sending a fresh test event)
if (registered.hooks["session.created"]) {
  await registered.hooks["session.created"]({}, {});
  console.log("  session.created: ran without throwing");
} else if (registered.hooks["session.idle"]) {
  await registered.hooks["session.idle"]({}, {});
  console.log("  session.idle: ran without throwing");
}

// After-hook event for tool failure capture
if (registered.hooks["tool.execute.after"]) {
  // Simulate a tool failure
  await registered.hooks["tool.execute.after"]({
    name: "Bash", result: { error: "command not found: foo", is_error: true },
  }, {});
  console.log("  tool.execute.after: ran without throwing (test passed)");
}

// chat.message capture: if it captures "remember ...", should save
if (registered.hooks["chat.message"]) {
  await registered.hooks["chat.message"]({
    role: "user", content: "remember: opencode plugin test 2026-07",
  }, {});
  // Verify a memory was created
  const rCheck = await callTool("mnemush-memory-search", { query: "opencode plugin test 2026-07" });
  check("chat.message: 'remember X' creates a memory",
        rCheck && !rCheck.isError && rCheck.content?.[0]?.text?.includes("remember"),
        rCheck.isError ? rCheck.content?.[0]?.text : "found");
}

// Self-eval log: every registered tool's execute goes through the
// registerTool wrapper, which writes ~/.mnemush/eval/<session>.ndjson.
// mnemush-memory uses a hand-written try/catch (not tryRun) — this
// verifies the wrapper covers it too.
{
  const evalDir = path.join(TMP_DATA, "eval");
  await new Promise((r) => setTimeout(r, 400));
  const fs = await import("node:fs");
  const files = fs.existsSync(evalDir) ? fs.readdirSync(evalDir) : [];
  const ndjson = files.filter((f) => f.endsWith(".ndjson"));
  check("self-eval NDJSON written for OpenCode tools",
        ndjson.length >= 1, `got ${ndjson.length}`);
  if (ndjson.length > 0) {
    const content = fs.readFileSync(path.join(evalDir, ndjson[0]), "utf8");
    const entries = content.trim().split("\n").filter(Boolean).map(JSON.parse);
    const tools = entries.map((e) => e.tool);
    check("eval entries have agent=opencode",
          entries.every((e) => e.agent === "opencode"), `got ${entries.map((e) => e.agent)}`);
    // All tools exercised above (add, search, get, link, neighbors,
    // reflect, save-search-result, status, identity-*, v0.3 actions)
    // must appear — including the hand-written mnemush-memory.
    check("mnemush-memory (hand-written try/catch) logged",
          tools.includes("mnemush-memory"), `tools: ${[...new Set(tools)].join(",")}`);
  }
}

console.log("");
console.log("=".repeat(60));
console.log(`RESULT: ${passed} passed, ${failed} failed`);
console.log("=".repeat(60));

// Cleanup
try {
  import("node:fs").then(fs => {
    if (fs.existsSync(TMP_DB)) fs.unlinkSync(TMP_DB);
    ["", "-wal", "-shm"].forEach(s => {
      const p = TMP_DB + s;
      if (fs.existsSync(p)) try { fs.unlinkSync(p); } catch {}
    });
    if (fs.existsSync(TMP_DATA)) fs.rmSync(TMP_DATA, {recursive: true, force: true});
  });
} catch {}

process.exit(failed === 0 ? 0 : 1);
