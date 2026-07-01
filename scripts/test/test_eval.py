"""TDD red phase: tests for mneme eval commands."""
import json, os, subprocess, sys, tempfile, time
from pathlib import Path


def setup_eval_dir(tmpdir):
    """Set up a fake ~/.mneme/eval/ with NDJSON log files."""
    mneme_root = Path(tmpdir) / ".mneme" / "eval"
    mneme_root.mkdir(parents=True)
    os.environ["HOME"] = str(tmpdir)
    return mneme_root


def make_log(eval_dir, session_id, entries):
    p = eval_dir / f"{session_id}.ndjson"
    with open(p, "w") as f:
        for e in entries:
            f.write(json.dumps(e) + "\n")
    return p


# ---- Test 1: stats with no eval dir ----
def test_stats_empty_dir():
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["HOME"] = tmp
        r = subprocess.run(
            ["$HOME/.cargo/bin/mneme", "eval", "stats"],
            capture_output=True, text=True, timeout=10,
        )
        assert r.returncode == 0, f"failed: {r.stderr}"
        out = r.stdout.lower()
        assert "0 calls" in out or "no eval data" in out, \
            f"expected empty-stats message, got: {r.stdout}"


# ---- Test 2: stats with entries ----
def test_stats_with_entries():
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["HOME"] = tmp
        eval_dir = setup_eval_dir(tmp)
        now = int(time.time())
        make_log(eval_dir, "session-1", [
            {"ts": now - 100, "session": "session-1", "agent": "pi",
             "tool": "memory_search", "args_summary": {"query": "jose"},
             "result_count": 3, "latency_ms": 12, "error": None},
            {"ts": now - 99, "session": "session-1", "agent": "pi",
             "tool": "memory_get", "args_summary": {"id": "abc"},
             "result_count": 1, "latency_ms": 5, "error": None},
            {"ts": now - 98, "session": "session-1", "agent": "pi",
             "tool": "memory_add", "args_summary": {"title": "t"},
             "result_count": 1, "latency_ms": 50, "error": None},
        ])
        make_log(eval_dir, "session-2", [
            {"ts": now - 50, "session": "session-2", "agent": "opencode",
             "tool": "mneme-memory-search", "args_summary": {"query": "auth"},
             "result_count": 0, "latency_ms": 8, "error": None},
            {"ts": now - 49, "session": "session-2", "agent": "opencode",
             "tool": "mneme-status", "args_summary": {},
             "result_count": 1, "latency_ms": 100, "error": "boom"},
        ])

        r = subprocess.run(
            ["$HOME/.cargo/bin/mneme", "eval", "stats"],
            capture_output=True, text=True, timeout=10,
        )
        assert r.returncode == 0, f"failed: {r.stderr}"
        out = r.stdout
        assert "5 total calls" in out or "5 calls" in out or "total: 5" in out.lower(), \
            f"expected 5 total calls, got: {out}"
        assert "memory_search" in out, f"missing per-tool breakdown: {out}"
        assert "mneme-status" in out, f"missing OpenCode tool: {out}"
        assert ("1 / 5" in out or "1 error" in out or "errors: 1" in out.lower()
                or "20.0%" in out), \
            f"missing error count: {out}"


# ---- Test 3: dump ----
def test_dump_ndjson():
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["HOME"] = tmp
        eval_dir = setup_eval_dir(tmp)
        now = int(time.time())
        make_log(eval_dir, "session-1", [
            {"ts": now - 100, "session": "session-1", "tool": "memory_search",
             "result_count": 3, "latency_ms": 12, "error": None},
        ])

        r = subprocess.run(
            ["$HOME/.cargo/bin/mneme", "eval", "dump"],
            capture_output=True, text=True, timeout=10,
        )
        assert r.returncode == 0
        lines = [l for l in r.stdout.split("\n") if l.strip()]
        assert len(lines) >= 1
        e = json.loads(lines[0])
        assert e["tool"] == "memory_search"


# ---- Test 4: stats --since filter ----
def test_stats_since_filter():
    with tempfile.TemporaryDirectory() as tmp:
        os.environ["HOME"] = tmp
        eval_dir = setup_eval_dir(tmp)
        now = int(time.time())
        # Old entry (1+ day ago) — should be filtered by --since 1d
        make_log(eval_dir, "old", [
            {"ts": now - 86400 - 100, "session": "old", "tool": "memory_search",
             "result_count": 0, "latency_ms": 5, "error": None},
        ])
        # New entry
        make_log(eval_dir, "new", [
            {"ts": now, "session": "new", "tool": "memory_search",
             "result_count": 1, "latency_ms": 5, "error": None},
        ])

        r = subprocess.run(
            ["$HOME/.cargo/bin/mneme", "eval", "stats", "--since", "1d"],
            capture_output=True, text=True, timeout=10,
        )
        assert r.returncode == 0
        assert "1 total calls" in r.stdout or "1 calls" in r.stdout or "total: 1" in r.stdout.lower(), \
            f"expected 1 call (only new), got: {r.stdout}"


if __name__ == "__main__":
    tests = [
        test_stats_empty_dir,
        test_stats_with_entries,
        test_dump_ndjson,
        test_stats_since_filter,
    ]
    passed = 0
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ✓ {t.__name__}")
            passed += 1
        except AssertionError as e:
            print(f"  ✗ {t.__name__}: {e}")
            failed += 1
        except Exception as e:
            print(f"  ✗ {t.__name__}: {type(e).__name__}: {e}")
            failed += 1
    print(f"\n{passed} passed, {failed} failed")
    sys.exit(0 if failed == 0 else 1)
