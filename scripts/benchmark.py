#!/usr/bin/env python3
"""Self-contained retrieval benchmark for mnemush.

Creates an isolated DB, seeds a 20-memory corpus across 5 topic clusters,
runs 15 queries, and reports IR metrics (recall@k, MRR, nDCG@k) on the
default `mnemush search` pipeline. Also runs a few extra sanity checks
(add-dedup, delete-visibility, reindex-idempotence).

Usage:
    python3 scripts/benchmark.py [--bin ~/.cargo/bin/mnemush] [--db /tmp/mnemush-bench.db]
    python3 scripts/benchmark.py --keep   # keep the DB for inspection
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

# ---------------------------------------------------------------------------
# Corpus: 5 topic clusters x 4 memories. Titles are unique and searchable;
# content carries cluster-specific keywords (plus cross-cluster noise words).
# ---------------------------------------------------------------------------

CLUSTERS = {
    "proxy": {
        "keywords": ["proxy", "github", "clash", "7890", "network"],
        "memories": [
            ("GitHub proxy setup", "Accessing GitHub requires the Clash proxy at 127.0.0.1:7890. Direct connections are flaky; curl ignores the system proxy unless HTTPS_PROXY is set. git itself is already configured with the proxy.", "lesson", ["proxy", "github"]),
            ("GitHub clone failure", "git clone from github.com failed repeatedly on direct connection. Fix: route through clash 127.0.0.1:7890 or use git config http.proxy. Remember curl needs explicit HTTPS_PROXY env.", "failure", ["proxy", "git"]),
            ("Clash port 7890", "The local Clash proxy listens on 127.0.0.1:7890 for both HTTP and SOCKS5. Set HTTPS_PROXY=http://127.0.0.1:7890 for command-line tools that ignore the system proxy.", "note", ["proxy"]),
            ("git remote mnemush", "After the repo rename, git remote set-url was used to point at git@github.com:Yunoinsky/mnemush.git. Pushes go through the configured proxy when needed.", "note", ["git", "github"]),
        ],
    },
    "embedding": {
        "keywords": ["embedding", "minimax", "embo-01", "1536", "vector"],
        "memories": [
            ("MiniMax embedding model", "Use the MiniMax CN API for embeddings. The embedding model is embo-01 (NOT minimax-m3, which is text generation). embo-01 produces 1536-dim vectors and uses the user's free quota.", "decision", ["embedding", "minimax"]),
            ("Embedding batch size", "The RemoteEmbedder posts to https://api.minimax.chat/v1/embeddings with batch size 10 and Bearer auth from MINIMAX_API_KEY. ureq agent has a 60s timeout to avoid hangs.", "note", ["embedding", "api"]),
            ("Embed bulk commit", "cli embed commits in chunks of 100 per transaction so a bulk run is resume-safe. 5614 memories embedded at 1536 dims.", "lesson", ["embedding"]),
            ("Semantic search check", "Semantic retrieval verified: query '访问 github 的网络代理设置' returns the proxy memory with score 0.99.", "note", ["embedding", "search"]),
        ],
    },
    "rename": {
        "keywords": ["rename", "mnemush", "mneme", "migration", "crates"],
        "memories": [
            ("Project rename mneme to mnemush", "The whole project was renamed mneme -> mnemush (mneme x mushroom portmanteau). crates/mneme became crates/mnemush; npm packages mneme-* became mnemush-*; data dir ~/.mneme became ~/.mnemush.", "decision", ["rename", "mnemush"]),
            ("Binary names after rename", "Binaries are now ~/.cargo/bin/mnemush and ~/.cargo/bin/mnemush-mcp. Old mneme binaries were removed after the rename.", "note", ["rename", "binary"]),
            ("Data dir migration", "The ~/.mneme data dir was migrated to ~/.mnemush. The config db_path had stayed pointing at the old dir and was fixed. Old dir deleted after verification.", "lesson", ["rename", "migration"]),
            ("Env var rename", "MNEME_* environment variables were renamed to MNEMUSH_*. Check docs and CI for stragglers referencing the old names.", "note", ["rename", "env"]),
        ],
    },
    "sleep": {
        "keywords": ["sleep", "drosophila", "fan-shaped", "dh44", "circadian"],
        "memories": [
            ("Dorsal fan-shaped body sleep", "Re-examining the role of the dorsal fan-shaped body in promoting sleep in Drosophila. Yue Hua co-authored the Curr Biol 2023 study from the UPenn Sehgal lab.", "note", ["sleep", "drosophila"]),
            ("Dh44 arousal neurons", "DN1a clock neurons connect to Dh44 arousal output neurons to drive consolidation of sleep in Drosophila L3 larvae. Glucose metabolic genes in Dh44 neurons drive sleep-wake rhythm development.", "note", ["sleep", "drosophila"]),
            ("Circadian rhythm connectome", "Using the FlyWire whole-brain connectome, the first comprehensive map of the circadian clock network was built: ~240 neurons with extensive contralateral connections.", "note", ["circadian", "connectome"]),
            ("Sleep and memory in flies", "Circadian sleep patterns enable long-term memory in Drosophila. Mature sleep-wake rhythms emerge at the L3 larval stage when the clock-arousal circuit forms.", "note", ["sleep", "memory"]),
        ],
    },
}

# Queries: (query, relevant memory titles, score-if-hit=3, distractor keywords)
# Each query targets one cluster. Some include cross-cluster noise.
QUERIES = [
    # proxy cluster
    ("how to access github from china", ["GitHub proxy setup", "GitHub clone failure", "git remote mnemush"], 3, ["china"]),
    ("clash proxy port", ["Clash port 7890", "GitHub proxy setup"], 3, []),
    ("git push fails proxy", ["GitHub proxy setup", "git remote mnemush"], 3, ["push"]),
    # embedding cluster
    ("minimax embedding model id", ["MiniMax embedding model", "Embedding batch size"], 3, []),
    ("embed all memories command", ["Embed bulk commit", "Semantic search check"], 3, ["command"]),
    ("vector dimension embo", ["MiniMax embedding model"], 3, ["dimension"]),
    # rename cluster
    ("project rename", ["Project rename mneme to mnemush", "Binary names after rename"], 3, []),
    ("data directory migration", ["Data dir migration", "Project rename mneme to mnemush"], 3, []),
    ("environment variable old name", ["Env var rename"], 3, ["old"]),
    # sleep cluster
    ("drosophila sleep regulation", ["Dorsal fan-shaped body sleep", "Dh44 arousal neurons", "Sleep and memory in flies"], 3, []),
    ("circadian clock neurons", ["Circadian rhythm connectome", "Dh44 arousal neurons"], 3, []),
    ("fruit fly long term memory", ["Sleep and memory in flies", "Dh44 arousal neurons"], 3, ["fruit"]),
    # semantic-only queries: zero FTS overlap with the (English) corpus,
    # retrievable only via vector recall
    ("怎么访问被墙的网站", ["Clash port 7890", "GitHub proxy setup"], 3, []),
    ("给果蝇做睡眠实验", ["Dh44 arousal neurons", "Dorsal fan-shaped body sleep"], 3, []),
]

# `[score] title (category)  #id` (no project) or `[score] project: title (category)  #id`
TITLE_RE = re.compile(r"^\[\s*([\d.]+)\]\s+(?:(.+?):\s+)?(.+?)\s+#(\S+)")


def make_config(enabled):
    """Write a temp config with embeddings enabled/disabled; return its path."""
    src = os.path.expanduser("~/.mnemush/config.toml")
    text = open(src).read() if os.path.exists(src) else ""
    new_lines = []
    in_embed = False
    for line in text.splitlines():
        if line.strip() == "[embedding]":
            in_embed = True
            new_lines.append(line)
            continue
        if in_embed and re.match(r"^\s*enabled\s*=", line):
            new_lines.append(f"enabled = {str(enabled).lower()}")
            continue
        if in_embed and line.strip().startswith("["):
            in_embed = False
        new_lines.append(line)
    if not any(re.match(r"^\s*enabled\s*=", l) for l in new_lines):
        new_lines.append("\n[embedding]\nenabled = " + str(enabled).lower())
    fd, path = tempfile.mkstemp(prefix="mnemush-bench-", suffix=".toml")
    os.write(fd, ("\n".join(new_lines) + "\n").encode())
    os.close(fd)
    return path


def run(bin_path, db_path, *args, config=None):
    """Run `mnemush --db <db> <args>` and return (rc, stdout, stderr)."""
    cmd = [bin_path, "--db", db_path]
    if config:
        cmd += ["--config", config]
    cmd += list(args)
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    return proc.returncode, proc.stdout, proc.stderr


def seed_corpus(bin_path, db_path):
    ids = {}
    for cluster, spec in CLUSTERS.items():
        for title, content, category, tags in spec["memories"]:
            rc, out, err = run(bin_path, db_path, "add", title, content,
                               "--category", category, "--tags", ",".join(tags))
            if rc != 0:
                sys.exit(f"add failed for {title}: {err}")
            # add prints the id on success
            m = re.search(r"([0-9a-f]{8}-[0-9a-f-]+)", out)
            if not m:
                sys.exit(f"cannot parse id from add output: {out!r}")
            ids[title] = m.group(1)
    return ids


def embed_corpus(bin_path, db_path, config):
    """Embed all memories in the benchmark DB (needs MINIMAX_API_KEY)."""
    key = os.environ.get("MINIMAX_API_KEY")
    if not key:
        mmx = os.path.expanduser("~/.mmx/config.json")
        if os.path.exists(mmx):
            key = json.load(open(mmx)).get("api_key")
    if not key:
        sys.exit("semantic mode needs MINIMAX_API_KEY (env or ~/.mmx/config.json)")
    os.environ["MINIMAX_API_KEY"] = key
    rc, out, err = run(bin_path, db_path, "embed", config=config)
    if rc != 0:
        sys.exit(f"embed failed: {err}")


def search(bin_path, db_path, query, limit=10, config=None):
    rc, out, err = run(bin_path, db_path, "search", query, "-l", str(limit), config=config)
    if rc != 0:
        return []
    hits = []
    for line in out.splitlines():
        m = TITLE_RE.match(line.strip())
        if m:
            title = re.sub(r"\s+\([^)]*\)\s*$", "", m.group(3).strip())
            hits.append((float(m.group(1)), title, m.group(4)))
    return hits

def dcg(relevances):
    return sum(rel / (i + 2) for i, rel in enumerate(relevances))  # log2(i+2)


def run_queries(bin_path, db_path, ids, config=None):
    """Run all queries; return per-query relevance lists and scores."""
    rows = []
    for query, relevant, rel_score, _noise in QUERIES:
        hits = search(bin_path, db_path, query, limit=10, config=config)
        title_to_rank = {t: i for i, (_, t, _) in enumerate(hits)}
        rel = [0] * len(hits)
        for title in relevant:
            if title in title_to_rank:
                rel[title_to_rank[title]] = rel_score
        rows.append((query, hits, rel))
    return rows


def metrics(rows, ks=(1, 3, 5)):
    """recall@k, MRR, nDCG@k across queries, for every k in ks."""
    n = len(rows)
    out = {}
    for k in ks:
        recall_sum = mrr_sum = ndcg_sum = 0.0
        for query, hits, rel in rows:
            top = rel[:k]
            n_rel = sum(1 for r in rel if r > 0)
            recall = sum(1 for r in top if r > 0) / n_rel if n_rel else 0.0
            rr = 0.0
            for i, r in enumerate(rel):
                if r > 0:
                    rr = 1.0 / (i + 1)
                    break
            ideal = sorted(rel, reverse=True)
            ndcg = dcg(top) / dcg(ideal[:k]) if dcg(ideal[:k]) else 0.0
            recall_sum += recall
            mrr_sum += rr
            ndcg_sum += ndcg
        out[f"recall@{k}"] = recall_sum / n
        out["MRR"] = mrr_sum / n
        out[f"nDCG@{k}"] = ndcg_sum / n
    return out


def extra_checks(bin_path, db_path, ids):
    """Sanity checks: add-dedup, delete-visibility, reindex idempotence."""
    results = []
    # 1. add with identical content returns the same id (dedup by content hash)
    rc, out, _ = run(bin_path, db_path, "add", "Duplicate title", "identical content body for dedup test")
    m = re.search(r"([0-9a-f]{8}-[0-9a-f-]+)", out)
    first = m.group(1) if m else None
    rc, out, _ = run(bin_path, db_path, "add", "Duplicate title", "identical content body for dedup test")
    m = re.search(r"([0-9a-f]{8}-[0-9a-f-]+)", out)
    second = m.group(1) if m else None
    results.append(("add-dedup-same-id", first is not None and first == second))

    # 2. deleted memory disappears from search
    victim = ids["Clash port 7890"]
    rc, _, _ = run(bin_path, db_path, "delete", victim)
    hits = search(bin_path, db_path, "clash port 7890", limit=5)
    gone = not any(h[2] == victim for h in hits)
    results.append(("delete-hides-from-search", rc == 0 and gone))

    # 3. reindex is idempotent (second run returns ok, no crash)
    rc1, _, err1 = run(bin_path, db_path, "reindex")
    rc2, _, err2 = run(bin_path, db_path, "reindex")
    results.append(("reindex-idempotent", rc1 == 0 and rc2 == 0))

    return results


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", default=os.path.expanduser("~/.cargo/bin/mnemush"))
    ap.add_argument("--db", default="")
    ap.add_argument("--mode", choices=["fts", "semantic", "both"], default="both",
                    help="fts = BM25-only; semantic = embedding-blend; both = compare (default)")
    ap.add_argument("--keep", action="store_true", help="keep the benchmark DB")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        sys.exit(f"binary not found: {args.bin}")
    if args.db:
        db_path = args.db
    else:
        fd, db_path = tempfile.mkstemp(prefix="mnemush-bench-", suffix=".db")
        os.close(fd)
        os.unlink(db_path)  # let mnemush create it fresh

    temp_configs = []
    try:
        ids = seed_corpus(args.bin, db_path)
        print(f"seeded {len(ids)} memories\n")

        modes = ["fts", "semantic"] if args.mode == "both" else [args.mode]
        results = {}
        for mode in modes:
            cfg = make_config(enabled=(mode == "semantic"))
            temp_configs.append(cfg)
            if mode == "semantic":
                print(f"[{mode}] embedding corpus ...")
                embed_corpus(args.bin, db_path, cfg)
            rows = run_queries(args.bin, db_path, ids, config=cfg)
            results[mode] = metrics(rows)
            print(f"[{mode}] {results[mode]}")

        if args.mode == "both":
            print("\ncompare (semantic vs fts):")
            for k in (1, 3, 5):
                for metric in (f"recall@{k}", "MRR", f"nDCG@{k}"):
                    fts = results["fts"][metric]
                    sem = results["semantic"][metric]
                    delta = sem - fts
                    arrow = "▲" if delta > 0.005 else ("▼" if delta < -0.005 else "=")
                    print(f"  {metric:9s}  fts={fts:.3f}  semantic={sem:.3f}  {arrow} {delta:+.3f}")

        print("\nextra checks:")
        for name, ok in extra_checks(args.bin, db_path, ids):
            print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    finally:
        for cfg in temp_configs:
            os.unlink(cfg)
        if not args.keep and not args.db:
            os.unlink(db_path)


if __name__ == "__main__":
    main()
