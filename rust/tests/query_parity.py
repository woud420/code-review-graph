"""Compare graph reads on a real Python-built index, including routes and tests."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile

from mcp_contract import WireClient
from public_parity import pinned_reference, run


def wire_reports(command, root, env):
    reports = [
        ("get_architecture_overview_tool", {}),
        ("get_hub_nodes_tool", {}), ("get_bridge_nodes_tool", {}),
        ("get_knowledge_gaps_tool", {}), ("get_surprising_connections_tool", {}),
        ("get_affected_flows_tool", {"changed_files": ["go.ts", "lib.py"]}),
    ]
    cases = [(name, dict(args, detail_level=mode))
             for name, args in reports for mode in ("standard", "minimal")]
    cases.append(("get_suggested_questions_tool", {}))
    cases.extend(("get_review_context_tool", opts) for opts in (
        {"changed_files": ["go.ts"]}, {"changed_files": ["lib.py"], "include_source": False},
        {"changed_files": ["lib.py"], "max_lines_per_file": 2, "max_results": 1},
        {"changed_files": ["lib.py", "absent"], "detail_level": "minimal"},
        {"changed_files": ["absent"]}, {"changed_files": []},
    ))
    cases.extend(("cross_repo_search_tool", dict(query="job", **opts)) for opts in (
        {}, {"repos": ["one", "unknown"]}, {"repos": ["unknown"]}, {"max_results": 1},
    ))
    client = WireClient(command + ["serve", "--repo", str(root)], root, env)
    try:
        client.call("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                  "clientInfo": {"name": "oracle", "version": "1"}})
        client.send({"method": "notifications/initialized"})
        return [(name, args, client.call("tools/call", {"name": name, "arguments": args}))
                for name, args in cases]
    finally:
        client.close()


def check(binary, source):
    pinned_reference(source)
    with tempfile.TemporaryDirectory(prefix="query-parity-") as tmp:
        root = Path(tmp).resolve() / "project"
        (root / "tests").mkdir(parents=True)
        files = {
            "lib.py": "class Base:\n    def action(self):\n        return job()\n\n"
                      "class Child(Base):\n    pass\n\ndef job():\n    return 1\n\n"
                      "def forward():\n    return job()\n",
            "tests/test_code.py": "from lib import job, forward\n\ndef test_job():\n"
                                  "    assert job()\n\ndef test_forward():\n    assert forward()\n",
            "go.ts": "export function service() { return 1; }\n",
            "client.ts": "import {service} from './go';\nexport function client() { return service(); }\n",
        }
        for name, code in files.items():
            (root / name).write_text(code, encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTHON", "CRG_", "FASTMCP_"))}
        env.update(CRG_HOME=str(root.parent / "registry"), TZ="UTC", NO_COLOR="1")
        ref_env = dict(env, PYTHONPATH=str(source), PYTHONDONTWRITEBYTECODE="1")
        reference = [sys.executable, "-m", "code_review_graph"]
        build = run(reference + ["build", "--repo", str(root)], root, ref_env)
        assert build["exit"] == 0, build
        with sqlite3.connect(root / ".code-review-graph/graph.db") as db:
            db.execute("UPDATE metadata SET value='2099-01-01T00:00:00+00:00' WHERE key='last_updated'")
        db.close()
        paths = [("client", "service"), ("forward", "job"), ("job", "forward"),
                 ("job", "job"), ("job", "absent"), ("client.ts", "go.ts")]
        cases = [["query", "path_to", a, "--to", b] for a, b in paths]
        cases += [["query", "tests_for", n] for n in ("job", "forward", "Base", "lib.py", "service")]
        cases += [["query", "importers_of", "go.ts"], ["query", "importers_of", "service"],
                  ["query", "inheritors_of", "Base"], ["query", "consumers_of", "some.setting"],
                  ["flows"], ["communities"], ["community", "--id", "1", "--members"]]
        cases.extend(["impact", "--files", changed, "--detail-level", mode, "--max-results", "1"]
                     for changed in ("lib.py", "go.ts", "absent") for mode in ("standard", "minimal", "compact"))
        for case in cases:
            args = [*case, "--repo", str(root)]
            want = run(reference + args, root, ref_env)
            got = run([str(binary), *args], root, env)
            assert want == got, (case, want, got)
            if case == ["query", "tests_for", "forward"]:
                rows = json.loads(want["stdout"])["results"]
                assert {(n["name"], n["indirect"]) for n in rows} == {("test_forward", False), ("test_job", True)}, rows
            if case == ["query", "path_to", "job", "--to", "forward"]:
                assert json.loads(want["stdout"])["reverse_route_exists"]
        registered = run(reference + ["register", str(root), "--alias", "one"], root, ref_env)
        assert registered["exit"] == 0, registered
        want = wire_reports(reference, root, ref_env)
        got = wire_reports([str(binary)], root, env)
        for (name, args, expected), (_, _, actual) in zip(want, got, strict=True):
            assert expected == actual, (name, args, expected, actual)
        print(f"{len(want)} analysis and multi-repository MCP wire responses matched")
        print(f"{len(cases)} graph query and report subprocess comparisons matched")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    args = parser.parse_args()
    check(args.binary.resolve(), args.upstream.resolve())
