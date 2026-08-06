#!/usr/bin/env python3
"""Smoke test for the mnemush-mcp server.

Spawns mnemush-mcp, sends a series of JSON-RPC requests, prints responses.

Covers all 12 MCP tools at least once:
  v0.1 (smoke baseline):  memory_add, memory_search, memory_link,
                           memory_neighbors, memory_get, scanner
  v0.2 (auto-maintenance): mnemush_status, memory_reflect,
                           memory_save_search_result, identity_propose
"""
import json
import os
import shutil
import subprocess
import sys

# Windows consoles default to cp1252 which can't encode ✓/× — force
# UTF-8 so the smoke test doesn't die on a print.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

DB_PATH = os.environ.get("MNEMUSH_DB_PATH", "/tmp/mnemush-mcp-smoke.db")
DATA_DIR = os.environ.get("MNEMUSH_DATA_DIR", "/tmp/mnemush-mcp-smoke-data")
for ext in ("", "-wal", "-shm"):
    p = DB_PATH + ext
    if os.path.exists(p):
        os.remove(p)
if os.path.isdir(DATA_DIR):
    shutil.rmtree(DATA_DIR)

env = os.environ.copy()
env["MNEMUSH_DB_PATH"] = DB_PATH
env["MNEMUSH_DATA_DIR"] = DATA_DIR

proc = subprocess.Popen(
    ["mnemush-mcp.exe" if os.name == "nt" else "mnemush-mcp"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    env=env,
    text=True,
    bufsize=1,
)


def send(req):
    proc.stdin.write(json.dumps(req) + "\n")
    proc.stdin.flush()


def recv():
    line = proc.stdout.readline()
    if not line:
        err = proc.stderr.read()
        raise RuntimeError(f"server closed: {err}")
    return json.loads(line)


def call(name, arguments):
    send({"jsonrpc": "2.0", "id": next_id(), "method": "tools/call",
          "params": {"name": name, "arguments": arguments}})
    return recv()


_id_counter = [9]


def next_id():
    _id_counter[0] += 1
    return _id_counter[0]


# 1. initialize
send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                 "clientInfo": {"name": "smoke-test", "version": "0.2.0"}}})
init = recv()
print(f"✓ initialize: server={init['result']['serverInfo']}")

# 2. notifications/initialized (server echoes a response)
send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
try:
    recv()
except Exception:
    pass

# 3. tools/list
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tools = recv()
tool_names = sorted(t['name'] for t in tools['result']['tools'])
print(f"✓ tools: {tool_names}")

# 4. memory_add
r = call("memory_add", {"title": "smoke", "content": "test via python stdio smoke test",
                        "importance": 0.7, "category": "decision"})
text = r["result"]["content"][0]["text"]
print(f"✓ add: {text}")
add_result = json.loads(text)
add_id = add_result.get("id")
assert add_id, f"could not parse add id from response: {text}"

# 5. memory_search
r = call("memory_search", {"query": "smoke"})
hits_text = r["result"]["content"][0]["text"]
print(f"✓ search: {hits_text[:100]}{'...' if len(hits_text) > 100 else ''}")
if hits_text.startswith("["):
    hits_list = json.loads(hits_text)
    assert any("smoke" in h.get("memory", {}).get("content", "") for h in hits_list), \
        f"expected smoke in results, got: {hits_text}"
else:
    assert "smoke" in hits_text or "no matches" in hits_text, f"got: {hits_text}"

# 6. memory_link
r = call("memory_link", {"source_id": add_id, "target_id": add_id,
                          "edge_type": "related", "strength": 0.5})
print(f"✓ link: {r['result']['content'][0]['text']}")

# 7. memory_neighbors
r = call("memory_neighbors", {"id": add_id, "max_hops": 1})
neighbors_text = r["result"]["content"][0]["text"]
neighbors = json.loads(neighbors_text) if not neighbors_text.startswith("(no") else []
print(f"✓ neighbors: {len(neighbors)} item(s)")

# 8. memory_get with invalid id should error
r = call("memory_get", {"id": "nonexistent"})
err_text = r.get("result", {}).get("content", [{}])[0].get("text", "")
is_err = r.get("result", {}).get("isError", False)
print(f"✓ invalid get: isError={is_err}, text={err_text!r}")
assert is_err, "expected isError=True for invalid id"
assert "not found" in err_text.lower(), f"expected 'not found', got: {err_text}"

# 9. scan blocks secret
r = call("memory_add", {"title": "secret", "content": "my AWS key is AKIAIOSFODNN7EXAMPLE"})
text = r["result"]["content"][0]["text"]
is_err = r.get("result", {}).get("isError", False)
print(f"✓ scan: isError={is_err}, text={text[:80]}")
assert is_err, "expected isError=True for secret-blocked add"
assert "scan" in text.lower() or "blocked" in text.lower(), f"expected scan block, got: {text}"

# 10. mnemush_status (v0.2)
r = call("mnemush_status", {})
status_text = r["result"]["content"][0]["text"]
print(f"✓ status: {status_text[:120]}{'...' if len(status_text) > 120 else ''}")
# Should mention active memory count and edge count
assert "active" in status_text.lower() or "memories" in status_text.lower(), \
    f"expected status summary, got: {status_text}"

# 11. memory_reflect (v0.2)
r = call("memory_reflect", {"since_days": 7, "limit": 5})
reflect_text = r["result"]["content"][0]["text"]
print(f"✓ reflect: {reflect_text[:120]}{'...' if len(reflect_text) > 120 else ''}")
# Should return JSON array of candidates (the just-added memory qualifies)
assert reflect_text.startswith("[") or "candidate" in reflect_text.lower() or \
    "no" in reflect_text.lower(), f"unexpected reflect output: {reflect_text[:200]}"

# 12. memory_save_search_result (v0.2)
r = call("memory_save_search_result", {"ids": [add_id], "query": "smoke",
                                        "category": "note", "importance": 0.5})
save_text = r["result"]["content"][0]["text"]
print(f"✓ save_search_result: {save_text[:120]}")
# Should report {saved: [...ids], errors: [...]}
assert "saved" in save_text.lower() or add_id in save_text, \
    f"expected saved id list, got: {save_text}"

# 13. memory_save_search_result with missing query must error (regression check)
# MCP errors come in two shapes: tool-level (result.isError=true) or
# JSON-RPC-level (top-level error.code). Check both.
r = call("memory_save_search_result", {"ids": [add_id]})
top_err = r.get("error")
is_err = r.get("result", {}).get("isError", False) or top_err is not None
err_msg = top_err.get("message", "") if top_err else r.get("result", {}).get("content", [{}])[0].get("text", "")
print(f"✓ save_search_result (no query): isError={is_err}, msg={err_msg!r}")
assert is_err, "expected error when query missing"
assert "query" in err_msg.lower(), f"expected query-related error, got: {err_msg}"

# 14. identity_propose (v0.2) — writes to pending.jsonl
r = call("identity_propose", {"target": "USER.md",
                               "content": "smoke test user note",
                               "reason": "added by test-mcp.py",
                               "evidence_count": 1})
prop_text = r["result"]["content"][0]["text"]
print(f"✓ identity_propose: {prop_text[:120]}")
# Should return a JSON object with id, target, status=pending
prop_obj = json.loads(prop_text) if prop_text.startswith("{") else {}
assert prop_obj.get("target") == "USER.md", f"unexpected propose output: {prop_text}"
assert prop_obj.get("status") == "pending", f"expected status=pending, got: {prop_obj}"
proposal_id = prop_obj.get("id")
assert proposal_id, f"expected proposal id, got: {prop_obj}"

# 15. identity_list_pending — should find the proposal from step 14
r = call("identity_list_pending", {})
list_text = r["result"]["content"][0]["text"]
print(f"✓ identity_list_pending: {list_text[:120]}")
assert proposal_id in list_text or "USER.md" in list_text, \
    f"expected to find proposal {proposal_id}, got: {list_text}"

# 16. identity_reject — clears the proposal so the test is idempotent
r = call("identity_reject", {"id": proposal_id})
reject_text = r["result"]["content"][0]["text"]
print(f"✓ identity_reject: {reject_text[:120]}")
assert "rejected" in reject_text.lower() or proposal_id in reject_text, \
    f"expected reject confirmation, got: {reject_text}"

proc.stdin.close()
proc.wait(timeout=5)
print("\n✓ all smoke tests passed")
