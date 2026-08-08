import { test } from "node:test";
import assert from "node:assert";
import { mkdtempSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { maybeRunDream } from "../src/index.ts";

test("maybeRunDream: fresh dataDir → triggers", async () => {
  const dir = mkdtempSync(join(tmpdir(), "dream-sched-"));
  const triggered = await maybeRunDream({ dataDir: dir });
  assert.equal(triggered, true, "fresh dir should trigger");
  assert.ok(existsSync(join(dir, "dream_last_run.json")), "state written");
});

test("maybeRunDream: just ran → skips within interval", async () => {
  const dir = mkdtempSync(join(tmpdir(), "dream-sched-"));
  const first = await maybeRunDream({ dataDir: dir });
  assert.equal(first, true);
  const second = await maybeRunDream({ dataDir: dir, minIntervalMs: 24 * 3600 * 1000 });
  assert.equal(second, false, "within interval should skip");
});

test("maybeRunDream: stale state → triggers again", async () => {
  const dir = mkdtempSync(join(tmpdir(), "dream-sched-"));
  writeFileSync(join(dir, "dream_last_run.json"), JSON.stringify({ last_run: Date.now() - 48 * 3600 * 1000 }));
  const triggered = await maybeRunDream({ dataDir: dir });
  assert.equal(triggered, true, "48h stale should trigger");
});
