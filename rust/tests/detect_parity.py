"""Compare full CLI reports and MCP wire responses before and after edits/deletions."""

import argparse
import os
from pathlib import Path
import sqlite3
import sys
import tempfile

from mcp_contract import WireClient
from public_parity import pinned_reference, run


def calls(prefix, repo, env, cases):
    client = WireClient([*prefix, "serve", "--repo", str(repo)], repo, env)
    try:
        client.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                   "clientInfo": {"name": "change-oracle", "version": "1"}})
        client.send({"method": "notifications/initialized"})
        return [client.call("tools/call", {"name": tool, "arguments": args}) for tool, args in cases]
    finally:
        client.close()


def compare(root, binary, source):
    pinned_reference(source)
    repo = root / "repo"
    repo.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_", "FASTMCP_"))}
    env.update(CRG_HOME=str(root / "registry"), NO_COLOR="1", TZ="UTC",
               GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
               GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid")
    ref_env = dict(env, PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1")
    py = [sys.executable, "-m", "code_review_graph"]
    rust = [str(binary)]

    def checked(command, environ=env):
        result = run(command, repo, environ)
        assert result["exit"] == 0, (command, result)
        return result

    def build():
        checked([*py, "build", "--repo", str(repo)], ref_env)
        with sqlite3.connect(repo / ".code-review-graph/graph.db") as db:
            db.execute("UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'")
        db.close()

    def compare_calls(cases):
        expected, actual = calls(py, repo, ref_env, cases), calls(rust, repo, env, cases)
        for (name, args), left, right in zip(cases, expected, actual, strict=True):
            assert left == right, (name, args, left, right)
        return [v["response"]["result"]["structuredContent"] for v in expected]

    checked(["git", "init", "-q"])
    edited = repo / "lib.py"
    edited.write_text("def outer():\n    return helper()\n\ndef helper():\n    return 2\n\n"
                      "def obsolete():\n    return 5\n", encoding="utf-8")
    (repo / "client.py").write_text("from lib import outer, obsolete\n\n"
                                     "def consumer():\n    return outer() + obsolete()\n", encoding="utf-8")
    (repo / ".gitignore").write_text(".code-review-graph/\n", encoding="utf-8")
    checked(["git", "add", "."])
    checked(["git", "commit", "-qm", "original code"])
    (repo / "notes.txt").write_text("notes\n", encoding="utf-8")
    checked(["git", "add", "."])
    checked(["git", "commit", "-qm", "notes"])
    minimal = "get_minimal_context_tool"
    missing, = compare_calls([(minimal, {"task": "onboard"})])
    assert missing["reason"] == "missing_graph", missing
    build()
    cli_count, wire_count = 0, 1
    for phase in ("clean", "edited", "deleted"):
        if phase == "edited":
            edited.write_text(edited.read_text().replace("return 2", "return 3"))
        elif phase == "deleted":
            edited.write_text(edited.read_text().replace("return 3", "return 2").replace("\ndef obsolete():\n    return 5\n", ""))
            build()
        for flags in ([], ["--churn"], ["--brief"], ["--details", "all"]):
            args = ["detect-changes", "--repo", str(repo), "--base", "HEAD", *flags]
            want, got = run(py + args, repo, ref_env), run(rust + args, repo, env)
            assert want == got, (phase, flags, want, got)
            cli_count += 1
        cases = [("detect_changes_tool", {"changed_files": ["lib.py"], "base": "HEAD", **opts})
                 for opts in ({"max_results": 1}, {"include_source": True}, {"detail_level": "minimal"})]
        cases.append(("detect_changes_tool", {"changed_files": []}))
        cases.extend((minimal, {"base": "HEAD", "task": task, "changed_files": ["lib.py"]})
                     for task in ("review", "debug", "refactor", "architecture", "other"))
        results = compare_calls(cases)
        wire_count += len(cases)
        if phase == "deleted":
            removed = results[0]["removed_symbols"]
            assert any(r["name"] == "obsolete" and r["used_by_total"] for r in removed), removed
    print(f"{cli_count} change CLI and {wire_count} MCP wire comparisons matched")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="change-parity-") as tmp:
        compare(Path(tmp).resolve(), args.binary.resolve(), args.upstream.resolve())
