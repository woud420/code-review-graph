"""Compare FTS and unindexed lexical search with the external Python reference.

Run tests/search_parity.py --upstream /path/to/pinned/source --binary /path/to/code-review-graph.
The interpreter must contain the reference package's locked dependencies.
"""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile

from differential import git
from mcp_contract import WireClient
from public_parity import Results, pinned_reference, run

CASES = {
    "exact": ["hashPassword"], "snake": ["hash_password"], "prefix": ["hash"],
    "multi-word": ["password hashing"], "or-widening": ["password absentterm"],
    "quoted": ['"hashing credentials"'], "punctuation": ["_"], "empty": [""],
    "absent": ["doesNotExistInFixture"], "class": ["hash", "--kind", "Class"],
    "limit-one": ["hash", "--limit", "1"], "unsupported-kind-data": ["hash", "--kind", "Output"],
}


def capture_mcp(cmd, repo, env):
    client = WireClient(cmd + ["serve", "--repo", str(repo)], repo, env)
    try:
        client.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                   "clientInfo": {"name": "search-check", "version": "1"}})
        client.send({"method": "notifications/initialized"})
        cases = {
            "minimal": {"query": "hash", "detail_level": "minimal", "limit": 1},
            "source": {"query": "hashPassword", "limit": 1, "include_source": True, "max_source_lines": 1},
            "neighbors": {"query": "hashPassword", "limit": 1, "include_source": True, "expand_neighbors": True},
            "absent": {"query": "noSuchMethodExists"},
            "kind": {"query": "hashPassword", "kind": "Class"},
        }
        return {name: client.call("tools/call", {"name": "semantic_search_nodes_tool", "arguments": arg})
                for name, arg in cases.items()}
    finally:
        client.close()


def check(binary, source, repo):
    pinned_reference(source)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_", "FASTMCP_"))}
    env.update(PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1", PYTHONHASHSEED="0", NO_COLOR="1",
               COLUMNS="80", TZ="UTC", CRG_SERIAL_PARSE="1", CRG_HOME=str(repo.parent / "registry"))
    rs_env = env.copy()
    del rs_env["PYTHONPATH"]
    python = [sys.executable, "-m", "code_review_graph"]
    rust = [str(binary)]
    git(repo, "init", "-q")
    result = Results()
    def cli(label, argv):
        ref = run(python + ["search", *argv, "--repo", str(repo)], repo, env)
        actual = run(rust + ["search", *argv, "--repo", str(repo)], repo, rs_env)
        result.check(label, ref, actual, cli=True)
        return ref
    cli("missing-index", ["hash"])
    (repo / "auth.py").write_text(
        'def hashPassword(text):\n    """Reliable hashing credentials"""\n    return text.strip()\n'
        'def check(value):\n    return hashPassword(value)\n'
        'class HashStore:\n    pass\n'
        + ''.join(f'def hash_password_{i}():\n    return {i}\n' for i in range(14)), encoding="utf-8")
    git(repo, "add", "--", "auth.py")
    git(repo, "commit", "-qm", "auth")
    for phase, flag in [("keyword", "--skip-postprocess"), ("fts", "--skip-flows")]:
        build = run(python + ["build", "--repo", str(repo), flag], repo, env)
        if build["exit"]:
            raise AssertionError(build)
        with sqlite3.connect(repo / ".code-review-graph/graph.db") as db:
            db.execute("UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'")
        db.close()
        for name, argv in CASES.items():
            obs = cli(f"{phase}/cli/{name}", argv)
            if obs["exit"]:
                raise AssertionError(obs)
            if name == "exact":
                data = json.loads(obs["stdout"])
                if data["search_mode"] != phase or data["results"][0]["name"] != "hashPassword":
                    raise AssertionError(f"reference {phase} ranking did not run: {data}")
            if name == "limit-one":
                data = json.loads(obs["stdout"])
                if not data["truncated"] or not data["total_is_lower_bound"]:
                    raise AssertionError("no real bounded result window was exercised")
        a = capture_mcp(python, repo, env)
        b = capture_mcp(rust, repo, rs_env)
        for name in a:
            result.check(f"{phase}/MCP/{name}", a[name], b[name])
        source_hit = a["neighbors"]["response"]["result"]["structuredContent"]["results"][0]
        if not source_hit["source"] or source_hit["callers"][0]["name"] != "check":
            raise AssertionError(f"reference has no usable neighbor snippet: {source_hit}")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="lexical-parity-") as tmp:
        repo = Path(tmp).resolve() / "corpus"
        repo.mkdir()
        outcomes = check(args.binary.resolve(), args.upstream.resolve(), repo)
    if args.report:
        args.report.write_text(json.dumps(outcomes.checks, indent=2) + "\n", encoding="utf-8")
    failed = [name for name, case in outcomes.checks.items() if not case["match"]]
    print(f"{len(outcomes.checks)-len(failed)}/{len(outcomes.checks)} search observations exactly matched")
    if failed:
        print("\n".join(failed))
    sys.exit(bool(failed))
