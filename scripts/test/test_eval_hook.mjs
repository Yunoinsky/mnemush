// Integration test: load mneme-pi's dist, exercise the after_tool_call
// hook, verify it writes one NDJSON line per mneme-related tool call.
//
// This is the E2E check that the eval log pipeline actually works
// end-to-end when triggered the way the pi runtime would trigger it.
import { fileURLToPath, pathToFileURL } from "node:url";
import { readFile, readdir, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";

const __filename = fileURLToPath(import.meta.url);
const __dirname = __filename.replace(/\/[^/]+$/, "");

// Path to the dist (built artifact under test)
const PI_DIST = "$HOME/Project/mneme/packages/mneme-pi/dist/index.js";

let passed = 0, failed = 0;
function check(name, ok, detail = "") {
  console.log(`  [${ok ? "✓" : "✗"}] ${name}${detail ? " — " + detail : ""}`);
  if (ok) passed++; else failed++;
}

// Test runner — each test rebuilds the env fresh.
async function withIsolatedHome(fn) {
  const tmpHome = await import("node:fs").then((fs) =>
    fs.promises.mkdtemp(join(tmpdir(), "mneme-eval-test-"))
  );
  const evalDir = join(tmpHome, ".mneme", "eval");
  // Override HOME so mneme-pi writes to ${HOME}/.mneme/eval
  const origHome = process.env.HOME;
  const origData = process.env.MNEME_DATA_DIR;
  process.env.HOME = tmpHome;
  process.env.MNEME_DATA_DIR = join(tmpHome, ".mneme");
  try {
    await fn(tmpHome, evalDir);
  } finally {
    process.env.HOME = origHome;
    process.env.MNEME_DATA_DIR = origData;
    // Best-effort cleanup
    try { await import("node:fs").then((fs) => fs.promises.rm(tmpHome, { recursive: true, force: true })); } catch {}
  }
}

// Build a fake `pi` API that captures every event registration.
function makeFakePi() {
  const handlers = new Map();
  const tools = [];
  return {
    handlers,
    tools,
    on(event, fn) { handlers.set(event, fn); },
    registerTool(def) { tools.push(def); },
    sendStatus(/* msg, ttlMs */) { /* no-op */ },
  };
}

// Re-import the mneme-pi extension module under test. The module
// imports from "mneme-client" which uses node_modules — we have to
// point require to the project's node_modules so the import resolves.
import { createRequire } from "node:module";
const require_fn = createRequire(import.meta.url);
function loadExtension() {
  try {
    // Resolve "mneme-client" to the project's copy
    const modulePath = require_fn.resolve("mneme-client", { paths: [
      "$HOME/Project/mneme/packages/mneme-pi",
    ] });
    // Bust the require cache so each test gets a fresh module
    delete require_fn.cache[PI_DIST];
    const mod = require_fn(PI_DIST);
    return mod.default || mod;
  } catch (e) {
    // Fallback: monkey-patch Module._resolveFilename to point
    // mneme-client at our local copy
    const Module = require_fn("node:module");
    const origResolve = Module._resolveFilename;
    Module._resolveFilename = function (request, parent, ...rest) {
      if (request === "mneme-client") {
        return "$HOME/Project/mneme/packages/mneme-client/dist/index.js";
      }
      return origResolve.call(this, request, parent, ...rest);
    };
    try {
      delete require_fn.cache[PI_DIST];
      const mod = require_fn(PI_DIST);
      return mod.default || mod;
    } finally {
      Module._resolveFilename = origResolve;
    }
  }
}

async function run() {
  const activate = loadExtension();

  // ── Test 1: mneme-memory-search hook fires ───────────────────────
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    check("after_tool_call hook registered", typeof handler === "function");
    // Trigger session_start to set the session id used by the eval hook
    const sessionStart = pi.handlers.get("session_start");
    if (sessionStart) await sessionStart();

    // Fire the hook with a memory_search event
    await handler({
      tool_name: "memory",
      args: { action: "search", query: "jose", limit: 5 },
      result: {
        content: [{ type: "text", text: '[{"memory":{"id":"abc","title":"x"}}]' }],
        isError: false,
      },
    });
    // Give async write a few ticks to flush
    await new Promise((r) => setTimeout(r, 200));

    // The hook creates the dir on first write. If reader sees ENOENT,
    // that's fine — the hook's mkdir() may have raced.
    let files = [];
    try {
      files = await readdir(evalDir);
    } catch (e) {
      if (e.code !== "ENOENT") throw e;
    }
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    check("eval log file created", ndjson.length === 1, `got ${ndjson.length}`);
    if (ndjson.length !== 1) return;
    const content = await readFile(join(evalDir, ndjson[0]), "utf8");
    const lines = content.trim().split("\n").filter(Boolean);
    check("log has 1 entry", lines.length === 1, `got ${lines.length}`);
    const entry = JSON.parse(lines[0]);
    check("entry has ts", typeof entry.ts === "number");
    check("entry has session", typeof entry.session === "string");
    check("entry.session starts with pi-", entry.session.startsWith("pi-"),
          entry.session);
    check("entry.tool == memory", entry.tool === "memory");
    check("entry.result_count == 1", entry.result_count === 1);
    check("entry.args_summary.query == 'jose'", entry.args_summary.query === "jose");
    check("entry.latency_ms is number", typeof entry.latency_ms === "number");
    check("entry.error is null", entry.error === null);
  });

  // ── Test 2: non-mneme tools are NOT logged ──────────────────────
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    await handler({
      tool_name: "Bash",  // not a mneme tool
      args: { command: "ls" },
      result: { content: [{ text: "file1\nfile2" }], isError: false },
    });
    await new Promise((r) => setTimeout(r, 50));
    const files = await readdir(evalDir).catch(() => []);
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    check("non-mneme tools NOT logged", ndjson.length === 0,
          `log dir has ${ndjson.length} files`);
  });

  // ── Test 3: failed tool call captures error ───────────────────────
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    await handler({
      tool_name: "memory_get",
      args: { id: "00000000-0000-0000-0000-000000000000" },
      result: {
        content: [{ type: "text", text: "memory not found" }],
        isError: true,
      },
    });
    await new Promise((r) => setTimeout(r, 200));
    const files = await readdir(evalDir);
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    check("error captured in log", ndjson.length === 1);
    const content = await readFile(join(evalDir, ndjson[0]), "utf8");
    const entry = JSON.parse(content.trim());
    check("error text captured", entry.error === "memory not found" || entry.error?.includes("not found"),
          `error=${entry.error}`);
    check("result_count is 0 on error", entry.result_count === 0);
  });

  // ── Test 4: multiple tool calls in same session share session id ─
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    await handler({ tool_name: "memory", args: { action: "search", query: "x" },
                    result: { content: [{ text: "[]" }], isError: false } });
    await handler({ tool_name: "memory_get", args: { id: "abc" },
                    result: { content: [{ text: '{}' }], isError: false } });
    // Poll for the file to have both entries (async writes are serialized
    // via the evalWriters queue in the hook, but resolve sequentially).
    let content = null;
    let lines = null;
    for (let i = 0; i < 100; i++) {
      await new Promise((r) => setTimeout(r, 50));
      try {
        const files = await readdir(evalDir);
        const ndjson = files.filter((f) => f.endsWith(".ndjson"));
        if (ndjson.length === 0) continue;
        content = await readFile(join(evalDir, ndjson[0]), "utf8");
        lines = content.trim().split("\n").filter(Boolean);
        if (lines.length >= 2) break;
      } catch { /* retry */ }
    }
    const files = await readdir(evalDir);
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    check("multiple calls → 1 NDJSON file", ndjson.length === 1);
    if (ndjson.length === 1) {
      check("multiple calls → 2 entries", lines.length === 2, `got ${lines.length}`);
      const e1 = JSON.parse(lines[0]);
      const e2 = JSON.parse(lines[1]);
      check("same session id", e1.session === e2.session,
            `${e1.session} vs ${e2.session}`);
      check("different tools", e1.tool !== e2.tool);
    }
  });

  // ── Test 5: long args are truncated in args_summary ───────────────
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    await handler({
      tool_name: "memory",
      args: { action: "add", title: "t", content: "x".repeat(500) },
      result: { content: [{ text: '{"id":"abc"}' }], isError: false },
    });
    await new Promise((r) => setTimeout(r, 200));
    const files = await readdir(evalDir);
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    const content = await readFile(join(evalDir, ndjson[0]), "utf8");
    const entry = JSON.parse(content.trim());
    const content_field = entry.args_summary.content;
    check("long content truncated in args_summary",
          content_field.length < 200 && content_field.endsWith("…"),
          `len=${content_field.length}, end='${content_field.slice(-10)}'`);
  });

  // ── Test 6: CLI mneme eval stats reads the log ───────────────────
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    // Write 3 entries from 2 different "sessions"
    await handler({ tool_name: "memory", args: { action: "search", query: "x" },
                    result: { content: [{ text: "[]" }], isError: false } });
    await handler({ tool_name: "memory", args: { action: "add", title: "t", content: "c" },
                    result: { content: [{ text: '{"id":"x"}' }], isError: false } });
    await handler({ tool_name: "memory_get", args: { id: "x" },
                    result: { content: [{ text: "{}" }], isError: false } });
    await new Promise((r) => setTimeout(r, 200));

    // Run the actual mneme binary against this HOME
    const r = spawn(
      "$HOME/.cargo/bin/mneme",
      ["eval", "stats"],
      { env: { ...process.env, HOME: tmpHome, MNEME_DATA_DIR: join(tmpHome, ".mneme") },
        encoding: "utf8" },
    );
    let out = "";
    for await (const chunk of r.stdout) out += chunk;
    await new Promise((res) => r.on("exit", res));
    check("CLI mneme eval stats reads the log",
          out.includes("3 total calls"),
          `output: ${out.slice(0, 200)}`);
    check("CLI reports per-tool breakdown",
          out.includes("memory") && out.includes("memory_get"),
          `output: ${out.slice(0, 200)}`);
  });

  // ── Test 7: OpenCode-style hyphenated names are recognized ──────
  // Regression: the old hand-maintained allowlist used underscores
  // (`mneme-memory_search`) but OpenCode registers hyphenated names
  // (`mneme-memory-search`). None of the OpenCode tools matched, so
  // calling them in a Pi session self-triggered the nudge counter and
  // they were never logged to the eval NDJSON. isMnemeTool() prefix-
  // matches, so both spellings are covered.
  await withIsolatedHome(async (tmpHome, evalDir) => {
    const pi = makeFakePi();
    activate(pi);
    const handler = pi.handlers.get("after_tool_call");
    const sessionStart = pi.handlers.get("session_start");
    if (sessionStart) await sessionStart();
    await handler({
      tool_name: "mneme-memory-search",  // OpenCode spelling
      args: { query: "x" },
      result: { content: [{ text: "[]" }], isError: false },
    });
    await handler({
      tool_name: "mneme-memory-action-update",  // OpenCode v0.3 spelling
      args: { id: "abc", status: "completed" },
      result: { content: [{ text: "{}" }], isError: false },
    });
    await new Promise((r) => setTimeout(r, 200));
    const files = await readdir(evalDir);
    const ndjson = files.filter((f) => f.endsWith(".ndjson"));
    check("eval log written for OpenCode hyphenated tools",
          ndjson.length === 1, `got ${ndjson.length}`);
    const content = await readFile(join(evalDir, ndjson[0]), "utf8");
    const lines = content.trim().split("\n").filter(Boolean);
    check("both OpenCode tool calls logged", lines.length === 2,
          `got ${lines.length}`);
    const tools = lines.map((l) => JSON.parse(l).tool);
    check("mneme-memory-search logged",
          tools.includes("mneme-memory-search"), `got ${tools}`);
    check("mneme-memory-action-update logged",
          tools.includes("mneme-memory-action-update"), `got ${tools}`);
  });

  console.log(`\n${passed} passed, ${failed} failed`);
  process.exit(failed === 0 ? 0 : 1);
}

run().catch((e) => {
  console.error("Fatal:", e);
  process.exit(2);
});
