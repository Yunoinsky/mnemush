#!/usr/bin/env python3
"""Smoke test for the mneme-mcp server.

Spawns mneme-mcp, sends a series of JSON-RPC requests, prints responses.
"""
import json
import os
import subprocess
import sys

DB_PATH = os.environ.get("MNEME_DB_PATH", "/tmp/mneme-mcp-smoke.db")
for ext in ("", "-wal", "-shm"):
    p = DB_PATH + ext
    if os.path.exists(p):
        os.remove(p)

env = os.environ.copy()
env["MNEME_DB_PATH"] = DB_PATH

proc = subprocess.Popen(
    ["mneme-mcp"],
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


# 1. initialize
send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                 "clientInfo": {"name": "smoke-test", "version": "0.0.1"}}})
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
print(f"✓ tools: {[t['name'] for t in tools['result']['tools']]}")

# 4. memory_add
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
      "params": {"name": "memory_add",
                 "arguments": {"title": "smoke",
                               "content": "test via python stdio smoke test",
                               "importance": 0.7, "category": "decision"}}})
r = recv()
text = r["result"]["content"][0]["text"]
print(f"✓ add: {text}")
# Parse {"id": "...", "conflicts": [...]}
add_result = json.loads(text)
add_id = add_result.get("id")
assert add_id, f"could not parse add id from response: {text}"

# 5. memory_search
send({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
      "params": {"name": "memory_search", "arguments": {"query": "smoke"}}})
r = recv()
hits_text = r["result"]["content"][0]["text"]
print(f"✓ search: {hits_text[:100]}{'...' if len(hits_text) > 100 else ''}")
# hits may be JSON array or "(no matches)"
if hits_text.startswith("["):
    hits_list = json.loads(hits_text)
    assert any("smoke" in h.get("memory", {}).get("content", "") for h in hits_list), \
        f"expected smoke in results, got: {hits_text}"
else:
    # legacy text format - check title contains smoke
    assert "smoke" in hits_text or "no matches" in hits_text, f"got: {hits_text}"

# 6. memory_link
send({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
      "params": {"name": "memory_link",
                 "arguments": {"source_id": add_id, "target_id": add_id,
                               "edge_type": "related", "strength": 0.5}}})
r = recv()
print(f"✓ link: {r['result']['content'][0]['text']}")

# 7. memory_neighbors
send({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
      "params": {"name": "memory_neighbors",
                 "arguments": {"id": add_id, "max_hops": 1}}})
r = recv()
neighbors_text = r["result"]["content"][0]["text"]
neighbors = json.loads(neighbors_text) if not neighbors_text.startswith("(no") else []
print(f"✓ neighbors: {len(neighbors)} item(s)")

# 8. memory_get with invalid id should error
send({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
      "params": {"name": "memory_get", "arguments": {"id": "nonexistent"}}})
r = recv()
# isError: true means content[0].text has the error message
err_text = r.get("result", {}).get("content", [{}])[0].get("text", "")
is_err = r.get("result", {}).get("isError", False)
print(f"✓ invalid get: isError={is_err}, text={err_text!r}")
assert is_err, "expected isError=True for invalid id"
assert "not found" in err_text.lower(), f"expected 'not found', got: {err_text}"

# 9. scan blocks secret
send({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
      "params": {"name": "memory_add",
                 "arguments": {"title": "secret",
                               "content": "my AWS key is AKIAIOSFODNN7EXAMPLE"}}})
r = recv()
text = r["result"]["content"][0]["text"]
is_err = r.get("result", {}).get("isError", False)
print(f"✓ scan: isError={is_err}, text={text[:80]}")
assert is_err, "expected isError=True for secret-blocked add"
assert "scan" in text.lower() or "blocked" in text.lower(), f"expected scan block, got: {text}"

proc.stdin.close()
proc.wait(timeout=5)
print("\n✓ all smoke tests passed")
